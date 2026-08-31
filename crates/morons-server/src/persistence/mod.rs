mod backend;
mod credentials;
mod database;
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
        REQUEST_FINGERPRINT_BYTES, create_session_fingerprint, validate_display_name,
    },
};

pub use self::{
    run_types::{
        AcceptedRun, MessageId, Run, RunCancellationResult, RunFailureKind, RunId,
        RunModelSelection, RunOpenCodeService, RunState, SessionEvent, SessionEventCursor,
        SessionEventPage, SessionEventPayload, TranscriptCursor, TranscriptEntry, TranscriptPage,
    },
    types::{
        MutationRequestId, OpenCodeCredentialStatus, PersistenceError, PersistenceResourceLimit,
        Session, SessionCatalogEvent, SessionCatalogEventCursor, SessionCatalogEventPage,
        SessionId, SessionListCursor, SessionPage,
    },
};

pub(crate) use self::run_types::{
    ActivationOutcome, CompletedAssistant, DispatchOutcome, MAX_TRANSCRIPT_TEXT_BYTES,
    PrepareOperationOutcome, ProviderOperationFailureState, ProviderUsage, RunContext,
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

    pub async fn create_session(
        &self,
        request_id: MutationRequestId,
        display_name: Option<String>,
    ) -> Result<Session, PersistenceError> {
        if request_id.is_zero() {
            return Err(PersistenceError::InvalidInput {
                reason: "a mutation request identifier must not be all zeroes",
            });
        }
        validate_display_name(display_name.as_deref())?;
        let fingerprint = create_session_fingerprint(display_name.as_deref());
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::CreateSession {
                request_id,
                fingerprint,
                display_name,
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
    CreateSession {
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        display_name: Option<String>,
        response: oneshot::Sender<Result<Session, PersistenceError>>,
    },
    Run(RunWorkerRequest),
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
        match request {
            WorkerRequest::CreateSession {
                request_id,
                fingerprint,
                display_name,
                response,
            } => {
                let _ =
                    response.send(backend.create_session(request_id, fingerprint, display_name));
            }
            WorkerRequest::Run(request) => request.execute(&mut backend),
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
                event_notifications.send_if_modified(|current| {
                    if high_water > *current {
                        *current = high_water;
                        true
                    } else {
                        false
                    }
                });
            }
            Err(error) => eprintln!("delivery event notification failed: {error}"),
        }
    }
}

#[cfg(test)]
mod credential_tests;
#[cfg(test)]
mod run_tests;
#[cfg(test)]
mod tests;
