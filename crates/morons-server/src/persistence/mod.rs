mod backend;
mod compactions;
mod credentials;
mod database;
pub(crate) mod images;
mod local_commands;
mod paths;
mod run_types;
mod runs;
mod types;
mod workspace;

use std::{fmt, path::Path, thread};

use morons_protocol::ServerEndpoint;
use tokio::sync::{Mutex, MutexGuard, mpsc, oneshot, watch};

use self::{
    backend::Backend,
    credentials::StoredOpenCodeApiKey,
    runs::RunWorkerRequest,
    types::{
        MAX_SESSION_CATALOG_EVENT_PAGE_SIZE, MAX_SESSION_EVENT_PAGE_SIZE, MAX_SESSION_PAGE_SIZE,
        REQUEST_FINGERPRINT_BYTES, archive_session_fingerprint,
        create_session_with_directory_fingerprint, delete_session_fingerprint,
        rename_session_fingerprint, validate_display_name, validate_working_directory_path,
    },
};

pub use self::{
    run_types::{
        AcceptedLocalCommand, AcceptedRun, ImageAttachment, ImageAttachmentId,
        LocalCommandCancellationResult, LocalCommandId, LocalCommandStatus, MessageId, Run,
        RunCancellationResult, RunFailureKind, RunId, RunModelSelection, RunOpenCodeService,
        RunState, SessionEvent, SessionEventCursor, SessionEventPage, SessionEventPayload,
        ToolCallId, TranscriptCursor, TranscriptEntry, TranscriptPage,
    },
    types::{
        MutationRequestId, OpenCodeCredentialStatus, PersistenceError, PersistenceResourceLimit,
        ServerStopResult, Session, SessionCatalogEvent, SessionCatalogEventCursor,
        SessionCatalogEventKind, SessionCatalogEventPage, SessionId, SessionListCursor,
        SessionPage, ToolUncertaintyAcknowledgement, WorkspaceBlockReason, WorkspaceState,
        WorkspaceSummary,
    },
};

pub(crate) use self::types::{ExecutionTargetArch, ExecutionTargetOs};

pub(crate) use self::run_types::{
    ActivationOutcome, AssistantMessagePhase, CONTEXT_POLICY_VERSION, CommittedToolCall,
    CommittedToolTurn, CompactionOperationId, CompactionPlan, CompletedAssistant,
    CompletedToolTurn, ContextCheckpoint, ContextCheckpointId, DispatchOutcome,
    MAX_TRANSCRIPT_TEXT_BYTES, PrepareOperationOutcome, PreparedImageAttachment,
    ProviderOperationFailureState, ProviderUsage, RunContext, RunInputContext,
    SessionContextStatus, ToolOperationRecovery, conservative_input_token_estimate,
};

const WORKER_QUEUE_CAPACITY: usize = 64;

pub struct SessionStore {
    sender: Option<mpsc::Sender<WorkerRequest>>,
    worker: Option<thread::JoinHandle<()>>,
    credential_dispatch_lock: Mutex<()>,
    event_notifications: watch::Sender<u64>,
}

pub struct OpenCodeCredentialLease<'a> {
    api_key: StoredOpenCodeApiKey,
    generation: u64,
    _dispatch_guard: MutexGuard<'a, ()>,
}

impl OpenCodeCredentialLease<'_> {
    #[cfg(test)]
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn api_key_bytes(&self) -> &[u8] {
        self.api_key.as_bytes()
    }
}

impl fmt::Debug for OpenCodeCredentialLease<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenCodeCredentialLease")
            .field("api_key", &"[REDACTED]")
            .field("generation", &self.generation)
            .finish()
    }
}

impl SessionStore {
    pub fn open(server: &ServerEndpoint) -> Result<Self, PersistenceError> {
        Self::open_at(server.claim_persistence_root()?)
    }

    #[cfg(test)]
    pub(crate) fn open_for_test(application_root: &Path) -> Result<Self, PersistenceError> {
        Self::open_at(application_root)
    }

    fn open_at(application_root: &Path) -> Result<Self, PersistenceError> {
        let backend = Backend::open(application_root)?;
        let event_high_water = backend.delivery_event_high_water()?;
        let (event_notifications, _) = watch::channel(event_high_water);
        let notification_sender = event_notifications.clone();
        let (sender, receiver) = mpsc::channel(WORKER_QUEUE_CAPACITY);
        let worker = thread::Builder::new()
            .name("morons-persistence".to_owned())
            .spawn(move || run_worker(backend, receiver, notification_sender))?;
        Ok(Self {
            sender: Some(sender),
            worker: Some(worker),
            credential_dispatch_lock: Mutex::new(()),
            event_notifications,
        })
    }

    #[cfg(test)]
    pub async fn create_session(
        &self,
        request_id: MutationRequestId,
        display_name: Option<String>,
    ) -> Result<Session, PersistenceError> {
        let working_directory = std::env::current_dir()?
            .into_os_string()
            .into_string()
            .map_err(|_| PersistenceError::InvalidInput {
                reason: "the test working directory is not valid UTF-8",
            })?;
        self.create_session_at(request_id, display_name, working_directory)
            .await
    }

    pub async fn create_session_at(
        &self,
        request_id: MutationRequestId,
        display_name: Option<String>,
        working_directory: String,
    ) -> Result<Session, PersistenceError> {
        if request_id.is_zero() {
            return Err(PersistenceError::InvalidInput {
                reason: "a mutation request identifier must not be all zeroes",
            });
        }
        validate_display_name(display_name.as_deref())?;
        validate_working_directory_path(&working_directory)?;
        let fingerprint =
            create_session_with_directory_fingerprint(display_name.as_deref(), &working_directory);
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::CreateSession {
                request_id,
                fingerprint,
                display_name,
                working_directory,
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub async fn rename_session(
        &self,
        request_id: MutationRequestId,
        session_id: SessionId,
        display_name: String,
    ) -> Result<Session, PersistenceError> {
        if request_id.is_zero() {
            return Err(PersistenceError::InvalidInput {
                reason: "a mutation request identifier must not be all zeroes",
            });
        }
        validate_display_name(Some(&display_name))?;
        let fingerprint = rename_session_fingerprint(session_id, &display_name);
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::RenameSession {
                request_id,
                fingerprint,
                session_id,
                display_name,
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    #[cfg(test)]
    pub async fn set_session_archived(
        &self,
        request_id: MutationRequestId,
        session_id: SessionId,
        archived: bool,
    ) -> Result<Session, PersistenceError> {
        let (session, applied) = self
            .prepare_session_archive(request_id, session_id, archived)
            .await?;
        if applied {
            return Ok(session);
        }
        self.complete_session_archive(request_id).await
    }

    pub(crate) async fn prepare_session_archive(
        &self,
        request_id: MutationRequestId,
        session_id: SessionId,
        archived: bool,
    ) -> Result<(Session, bool), PersistenceError> {
        if request_id.is_zero() {
            return Err(PersistenceError::InvalidInput {
                reason: "a mutation request identifier must not be all zeroes",
            });
        }
        let fingerprint = archive_session_fingerprint(session_id, archived);
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::PrepareSessionArchive {
                request_id,
                fingerprint,
                session_id,
                archived,
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub(crate) async fn complete_session_archive(
        &self,
        request_id: MutationRequestId,
    ) -> Result<Session, PersistenceError> {
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::CompleteSessionArchive {
                request_id,
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    #[cfg(test)]
    pub async fn delete_session(
        &self,
        request_id: MutationRequestId,
        session_id: SessionId,
    ) -> Result<SessionId, PersistenceError> {
        let complete = self.prepare_session_delete(request_id, session_id).await?;
        if !complete {
            self.clean_session_database(request_id).await?;
            self.complete_session_delete(request_id).await
        } else {
            Ok(session_id)
        }
    }

    pub(crate) async fn prepare_session_delete(
        &self,
        request_id: MutationRequestId,
        session_id: SessionId,
    ) -> Result<bool, PersistenceError> {
        if request_id.is_zero() {
            return Err(PersistenceError::InvalidInput {
                reason: "a mutation request identifier must not be all zeroes",
            });
        }
        let fingerprint = delete_session_fingerprint(session_id);
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::PrepareSessionDelete {
                request_id,
                fingerprint,
                session_id,
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub(crate) async fn clean_session_database(
        &self,
        request_id: MutationRequestId,
    ) -> Result<(), PersistenceError> {
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::CleanSessionDatabase {
                request_id,
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub(crate) async fn complete_session_delete(
        &self,
        request_id: MutationRequestId,
    ) -> Result<SessionId, PersistenceError> {
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::CompleteSessionDelete {
                request_id,
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub async fn request_server_stop(
        &self,
        request_id: MutationRequestId,
        host_epoch: [u8; 16],
    ) -> Result<ServerStopResult, PersistenceError> {
        if request_id.is_zero() {
            return Err(PersistenceError::InvalidInput {
                reason: "a mutation request identifier must not be all zeroes",
            });
        }
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::StopServer {
                request_id,
                host_epoch,
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub async fn open_code_credential_status(
        &self,
    ) -> Result<OpenCodeCredentialStatus, PersistenceError> {
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::GetOpenCodeCredentialStatus {
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub async fn set_open_code_credential(
        &self,
        request_id: MutationRequestId,
        expected_generation: u64,
        api_key: Vec<u8>,
    ) -> Result<OpenCodeCredentialStatus, PersistenceError> {
        if request_id.is_zero() {
            return Err(PersistenceError::InvalidInput {
                reason: "a mutation request identifier must not be all zeroes",
            });
        }
        let api_key = StoredOpenCodeApiKey::new(api_key)?;
        let _dispatch_guard = self.credential_dispatch_lock.lock().await;
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::SetOpenCodeCredential {
                request_id,
                expected_generation,
                api_key,
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub async fn remove_open_code_credential(
        &self,
        request_id: MutationRequestId,
        expected_generation: u64,
    ) -> Result<OpenCodeCredentialStatus, PersistenceError> {
        if request_id.is_zero() {
            return Err(PersistenceError::InvalidInput {
                reason: "a mutation request identifier must not be all zeroes",
            });
        }
        let _dispatch_guard = self.credential_dispatch_lock.lock().await;
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::RemoveOpenCodeCredential {
                request_id,
                expected_generation,
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub async fn lease_open_code_credential(
        &self,
        expected_generation: u64,
    ) -> Result<OpenCodeCredentialLease<'_>, PersistenceError> {
        let dispatch_guard = self.credential_dispatch_lock.lock().await;
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::LeaseOpenCodeCredential {
                expected_generation,
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        let api_key = response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)??;
        Ok(OpenCodeCredentialLease {
            api_key,
            generation: expected_generation,
            _dispatch_guard: dispatch_guard,
        })
    }

    pub async fn get_session(
        &self,
        session_id: SessionId,
    ) -> Result<Option<Session>, PersistenceError> {
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::GetSession {
                session_id,
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub async fn list_sessions(
        &self,
        cursor: Option<SessionListCursor>,
        limit: u16,
    ) -> Result<SessionPage, PersistenceError> {
        if limit == 0 || limit > MAX_SESSION_PAGE_SIZE {
            return Err(PersistenceError::InvalidInput {
                reason: "a session page size must be between 1 and 100",
            });
        }
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::ListSessions {
                cursor,
                limit,
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub async fn read_session_catalog_events(
        &self,
        cursor: SessionCatalogEventCursor,
        limit: u16,
    ) -> Result<SessionCatalogEventPage, PersistenceError> {
        if limit == 0 || limit > MAX_SESSION_CATALOG_EVENT_PAGE_SIZE {
            return Err(PersistenceError::InvalidInput {
                reason: "a session event page size must be between 1 and 100",
            });
        }
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::ReadSessionCatalogEvents {
                cursor,
                limit,
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub async fn read_session_events(
        &self,
        session_id: SessionId,
        cursor: SessionEventCursor,
        limit: u16,
    ) -> Result<SessionEventPage, PersistenceError> {
        if limit == 0 || limit > MAX_SESSION_EVENT_PAGE_SIZE {
            return Err(PersistenceError::InvalidInput {
                reason: "a session event page size must be between 1 and 100",
            });
        }
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::ReadSessionEvents {
                session_id,
                cursor,
                limit,
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub(crate) fn subscribe_event_notifications(&self) -> watch::Receiver<u64> {
        self.event_notifications.subscribe()
    }

    fn sender(&self) -> Result<&mpsc::Sender<WorkerRequest>, PersistenceError> {
        self.sender.as_ref().ok_or(PersistenceError::WorkerStopped)
    }
}

impl Drop for SessionStore {
    fn drop(&mut self) {
        drop(self.sender.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

enum WorkerRequest {
    LocalCommand(local_commands::LocalCommandWorkerRequest),
    CreateSession {
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        display_name: Option<String>,
        working_directory: String,
        response: oneshot::Sender<Result<Session, PersistenceError>>,
    },
    RenameSession {
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        session_id: SessionId,
        display_name: String,
        response: oneshot::Sender<Result<Session, PersistenceError>>,
    },
    PrepareSessionArchive {
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        session_id: SessionId,
        archived: bool,
        response: oneshot::Sender<Result<(Session, bool), PersistenceError>>,
    },
    CompleteSessionArchive {
        request_id: MutationRequestId,
        response: oneshot::Sender<Result<Session, PersistenceError>>,
    },
    PrepareSessionDelete {
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        session_id: SessionId,
        response: oneshot::Sender<Result<bool, PersistenceError>>,
    },
    CleanSessionDatabase {
        request_id: MutationRequestId,
        response: oneshot::Sender<Result<(), PersistenceError>>,
    },
    CompleteSessionDelete {
        request_id: MutationRequestId,
        response: oneshot::Sender<Result<SessionId, PersistenceError>>,
    },
    Run(RunWorkerRequest),
    StopServer {
        request_id: MutationRequestId,
        host_epoch: [u8; 16],
        response: oneshot::Sender<Result<ServerStopResult, PersistenceError>>,
    },
    GetOpenCodeCredentialStatus {
        response: oneshot::Sender<Result<OpenCodeCredentialStatus, PersistenceError>>,
    },
    SetOpenCodeCredential {
        request_id: MutationRequestId,
        expected_generation: u64,
        api_key: StoredOpenCodeApiKey,
        response: oneshot::Sender<Result<OpenCodeCredentialStatus, PersistenceError>>,
    },
    RemoveOpenCodeCredential {
        request_id: MutationRequestId,
        expected_generation: u64,
        response: oneshot::Sender<Result<OpenCodeCredentialStatus, PersistenceError>>,
    },
    LeaseOpenCodeCredential {
        expected_generation: u64,
        response: oneshot::Sender<Result<StoredOpenCodeApiKey, PersistenceError>>,
    },
    GetSession {
        session_id: SessionId,
        response: oneshot::Sender<Result<Option<Session>, PersistenceError>>,
    },
    ListSessions {
        cursor: Option<SessionListCursor>,
        limit: u16,
        response: oneshot::Sender<Result<SessionPage, PersistenceError>>,
    },
    ReadSessionCatalogEvents {
        cursor: SessionCatalogEventCursor,
        limit: u16,
        response: oneshot::Sender<Result<SessionCatalogEventPage, PersistenceError>>,
    },
    ReadSessionEvents {
        session_id: SessionId,
        cursor: SessionEventCursor,
        limit: u16,
        response: oneshot::Sender<Result<SessionEventPage, PersistenceError>>,
    },
}

fn run_worker(
    mut backend: Backend,
    mut receiver: mpsc::Receiver<WorkerRequest>,
    event_notifications: watch::Sender<u64>,
) {
    while let Some(request) = receiver.blocking_recv() {
        let mut force_event_notification = false;
        match request {
            WorkerRequest::LocalCommand(request) => request.execute(&mut backend),
            WorkerRequest::CreateSession {
                request_id,
                fingerprint,
                display_name,
                working_directory,
                response,
            } => {
                let _ = response.send(backend.create_session(
                    request_id,
                    fingerprint,
                    display_name,
                    working_directory,
                ));
            }
            WorkerRequest::RenameSession {
                request_id,
                fingerprint,
                session_id,
                display_name,
                response,
            } => {
                let _ = response.send(backend.rename_session(
                    request_id,
                    fingerprint,
                    session_id,
                    display_name,
                ));
            }
            WorkerRequest::PrepareSessionArchive {
                request_id,
                fingerprint,
                session_id,
                archived,
                response,
            } => {
                let _ = response.send(backend.prepare_session_archive(
                    request_id,
                    fingerprint,
                    session_id,
                    archived,
                ));
            }
            WorkerRequest::CompleteSessionArchive {
                request_id,
                response,
            } => {
                let result = backend.complete_session_archive(request_id);
                force_event_notification = result.is_ok();
                let _ = response.send(result);
            }
            WorkerRequest::PrepareSessionDelete {
                request_id,
                fingerprint,
                session_id,
                response,
            } => {
                let _ = response.send(backend.prepare_session_delete(
                    request_id,
                    fingerprint,
                    session_id,
                ));
            }
            WorkerRequest::CleanSessionDatabase {
                request_id,
                response,
            } => {
                let _ = response.send(backend.clean_session_database(request_id));
            }
            WorkerRequest::CompleteSessionDelete {
                request_id,
                response,
            } => {
                let result = backend.complete_session_delete(request_id);
                force_event_notification = result.is_ok();
                let _ = response.send(result);
            }
            WorkerRequest::Run(request) => request.execute(&mut backend),
            WorkerRequest::StopServer {
                request_id,
                host_epoch,
                response,
            } => {
                let _ = response.send(backend.request_server_stop(request_id, host_epoch));
            }
            WorkerRequest::GetOpenCodeCredentialStatus { response } => {
                let _ = response.send(backend.open_code_credential_status());
            }
            WorkerRequest::SetOpenCodeCredential {
                request_id,
                expected_generation,
                api_key,
                response,
            } => {
                let _ = response.send(backend.set_open_code_credential(
                    request_id,
                    expected_generation,
                    api_key,
                ));
            }
            WorkerRequest::RemoveOpenCodeCredential {
                request_id,
                expected_generation,
                response,
            } => {
                let _ = response
                    .send(backend.remove_open_code_credential(request_id, expected_generation));
            }
            WorkerRequest::LeaseOpenCodeCredential {
                expected_generation,
                response,
            } => {
                let _ = response.send(
                    backend
                        .credentials
                        .clone_key_for_dispatch(expected_generation),
                );
            }
            WorkerRequest::GetSession {
                session_id,
                response,
            } => {
                let _ = response.send(backend.get_session(session_id));
            }
            WorkerRequest::ListSessions {
                cursor,
                limit,
                response,
            } => {
                let _ = response.send(backend.list_sessions(cursor, limit));
            }
            WorkerRequest::ReadSessionCatalogEvents {
                cursor,
                limit,
                response,
            } => {
                let _ = response.send(backend.read_session_catalog_events(cursor, limit));
            }
            WorkerRequest::ReadSessionEvents {
                session_id,
                cursor,
                limit,
                response,
            } => {
                let _ = response.send(backend.read_session_events(session_id, cursor, limit));
            }
        }
        match backend.delivery_event_high_water() {
            Ok(high_water) => {
                if force_event_notification {
                    // Archive acceptance reserves its catalog sequence before active work stops.
                    // Completion can therefore add an event below the global delivery high water.
                    event_notifications.send_replace(high_water);
                } else {
                    event_notifications.send_if_modified(|current| {
                        if high_water > *current {
                            *current = high_water;
                            true
                        } else {
                            false
                        }
                    });
                }
            }
            Err(error) => eprintln!("delivery event notification failed: {error}"),
        }
    }
}

#[cfg(test)]
mod credential_tests;
#[cfg(test)]
mod local_command_tests;
#[cfg(test)]
mod run_tests;
#[cfg(test)]
mod tests;
