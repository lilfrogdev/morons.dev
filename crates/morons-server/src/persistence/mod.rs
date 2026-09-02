mod backend;
mod commands;
mod credentials;
mod database;
mod execution_image;
mod execution_image_operation;
mod paths;
mod repository;
mod review;
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
        SessionEventPage, SessionEventPayload, ToolCallId, TranscriptCursor, TranscriptEntry,
        TranscriptPage,
    },
    types::{
        ExecutionImageState, ExecutionImageSummary, ExecutionTargetArch, ExecutionTargetOs,
        MutationRequestId, OpenCodeCredentialStatus, PersistenceError, PersistenceResourceLimit,
        ServerStopResult, Session, SessionCatalogEvent, SessionCatalogEventCursor,
        SessionCatalogEventPage, SessionId, SessionListCursor, SessionPage,
        ToolUncertaintyAcknowledgement, WorkspaceBlockReason, WorkspaceState, WorkspaceSummary,
    },
};

pub(crate) use self::types::{
    CommandResources, ExecutionImageOutcome, ExecutionImagePlan, ExportPlan,
    RepositoryImportOutcome, RepositoryImportPlan, ReviewResources, WorktreeLayoutPlan,
};

pub(crate) use self::run_types::{
    ActivationOutcome, AssistantMessagePhase, CommittedToolCall, CommittedToolTurn,
    CompletedAssistant, CompletedToolTurn, DispatchOutcome, MAX_TRANSCRIPT_TEXT_BYTES,
    PrepareOperationOutcome, ProviderOperationFailureState, ProviderUsage, RunContext,
    ToolOperationRecovery,
};

const WORKER_QUEUE_CAPACITY: usize = 64;

pub struct SessionStore {
    sender: Option<mpsc::Sender<WorkerRequest>>,
    worker: Option<thread::JoinHandle<()>>,
    credential_dispatch_lock: Mutex<()>,
    repository_import_lock: Mutex<()>,
    execution_image_lock: Mutex<()>,
    paths: paths::StoragePaths,
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
        let paths = backend.paths.clone();
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
            repository_import_lock: Mutex::new(()),
            execution_image_lock: Mutex::new(()),
            paths,
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

    pub(crate) async fn active_worktree_path(
        &self,
        workspace_id: [u8; 16],
    ) -> Result<std::path::PathBuf, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::GetActiveWorktreeGeneration {
                workspace_id,
                response,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        let generation_id = receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)??;
        Ok(self
            .paths
            .worktree_generation_path(&workspace_id, &generation_id))
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
    PrepareExecutionImage {
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        toolchain_source_digest: [u8; REQUEST_FINGERPRINT_BYTES],
        cargo_source_digest: [u8; REQUEST_FINGERPRINT_BYTES],
        response: oneshot::Sender<Result<ExecutionImagePlan, PersistenceError>>,
    },
    DispatchExecutionImage {
        plan: ExecutionImagePlan,
        response: oneshot::Sender<Result<ExecutionImagePlan, PersistenceError>>,
    },
    CompleteExecutionImage {
        plan: ExecutionImagePlan,
        outcome: ExecutionImageOutcome,
        response: oneshot::Sender<Result<ExecutionImageSummary, PersistenceError>>,
    },
    ExecutionImageNotApplied {
        plan: ExecutionImagePlan,
        response: oneshot::Sender<Result<ExecutionImageSummary, PersistenceError>>,
    },
    BlockExecutionImage {
        plan: ExecutionImagePlan,
        response: oneshot::Sender<Result<ExecutionImageSummary, PersistenceError>>,
    },
    GetExecutionImageSummary {
        response: oneshot::Sender<Result<ExecutionImageSummary, PersistenceError>>,
    },
    GetActiveWorktreeGeneration {
        workspace_id: [u8; 16],
        response: oneshot::Sender<Result<[u8; 16], PersistenceError>>,
    },
    GetCommandResources {
        run_id: RunId,
        workspace_id: [u8; 16],
        response: oneshot::Sender<Result<CommandResources, PersistenceError>>,
    },
    PrepareCommandOperation {
        run_id: RunId,
        call_id: ToolCallId,
        operation_id: run_types::ToolOperationId,
        resources: CommandResources,
        generation_id: [u8; 16],
        source: RepositoryImportOutcome,
        response:
            oneshot::Sender<Result<backend::command_execution::CommandBinding, PersistenceError>>,
    },
    CompleteCommandResult {
        run_id: RunId,
        call_id: ToolCallId,
        operation_id: run_types::ToolOperationId,
        result: crate::tools::ToolResult,
        publication: Option<(
            backend::command_execution::CommandBinding,
            RepositoryImportOutcome,
        )>,
        response: oneshot::Sender<Result<TranscriptEntry, PersistenceError>>,
    },
    PrepareExport {
        request_id: MutationRequestId,
        session_id: SessionId,
        generation_id: [u8; 16],
        destination_digest: [u8; 32],
        response: oneshot::Sender<Result<ExportPlan, PersistenceError>>,
    },
    DispatchExport {
        plan: ExportPlan,
        response: oneshot::Sender<Result<ExportPlan, PersistenceError>>,
    },
    CompleteExport {
        plan: ExportPlan,
        outcome: RepositoryImportOutcome,
        response: oneshot::Sender<Result<morons_protocol::ExportSummary, PersistenceError>>,
    },
    GetExportSummary {
        request_id: MutationRequestId,
        response: oneshot::Sender<Result<morons_protocol::ExportSummary, PersistenceError>>,
    },
    GetReviewResources {
        session_id: SessionId,
        response: oneshot::Sender<Result<ReviewResources, PersistenceError>>,
    },
    PrepareRepositoryImport {
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        source_path_digest: [u8; REQUEST_FINGERPRINT_BYTES],
        session_id: SessionId,
        response: oneshot::Sender<Result<RepositoryImportPlan, PersistenceError>>,
    },
    DispatchRepositoryImport {
        plan: RepositoryImportPlan,
        response: oneshot::Sender<Result<RepositoryImportPlan, PersistenceError>>,
    },
    CompleteRepositoryImport {
        plan: RepositoryImportPlan,
        outcome: RepositoryImportOutcome,
        response: oneshot::Sender<Result<WorkspaceSummary, PersistenceError>>,
    },
    RepositoryImportNotApplied {
        plan: RepositoryImportPlan,
        response: oneshot::Sender<Result<WorkspaceSummary, PersistenceError>>,
    },
    BlockRepositoryImport {
        plan: RepositoryImportPlan,
        response: oneshot::Sender<Result<WorkspaceSummary, PersistenceError>>,
    },
    GetWorkspaceSummary {
        session_id: SessionId,
        response: oneshot::Sender<Result<WorkspaceSummary, PersistenceError>>,
    },
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
            WorkerRequest::PrepareExecutionImage {
                request_id,
                fingerprint,
                toolchain_source_digest,
                cargo_source_digest,
                response,
            } => {
                let _ = response.send(backend.prepare_execution_image(
                    request_id,
                    fingerprint,
                    toolchain_source_digest,
                    cargo_source_digest,
                ));
            }
            WorkerRequest::DispatchExecutionImage { plan, response } => {
                let _ = response.send(backend.dispatch_execution_image(plan));
            }
            WorkerRequest::CompleteExecutionImage {
                plan,
                outcome,
                response,
            } => {
                let _ = response.send(backend.complete_execution_image(plan, outcome));
            }
            WorkerRequest::ExecutionImageNotApplied { plan, response } => {
                let _ = response.send(backend.mark_execution_image_not_applied(plan));
            }
            WorkerRequest::BlockExecutionImage { plan, response } => {
                let _ = response.send(backend.block_execution_image(plan));
            }
            WorkerRequest::GetExecutionImageSummary { response } => {
                let _ = response.send(backend.execution_image_summary());
            }
            WorkerRequest::GetActiveWorktreeGeneration {
                workspace_id,
                response,
            } => {
                let _ = response.send(backend.active_worktree_generation(&workspace_id));
            }
            WorkerRequest::GetCommandResources {
                run_id,
                workspace_id,
                response,
            } => {
                let _ = response.send(backend.command_resources(run_id, &workspace_id));
            }
            WorkerRequest::PrepareCommandOperation {
                run_id,
                call_id,
                operation_id,
                resources,
                generation_id,
                source,
                response,
            } => {
                let _ = response.send(backend.prepare_command_operation(
                    run_id,
                    call_id,
                    operation_id,
                    resources,
                    generation_id,
                    source,
                ));
            }
            WorkerRequest::CompleteCommandResult {
                run_id,
                call_id,
                operation_id,
                result,
                publication,
                response,
            } => {
                let _ = response.send(backend.complete_command_result(
                    run_id,
                    call_id,
                    operation_id,
                    result,
                    publication,
                ));
            }
            WorkerRequest::PrepareExport {
                request_id,
                session_id,
                generation_id,
                destination_digest,
                response,
            } => {
                let _ = response.send(backend.prepare_export(
                    request_id,
                    session_id,
                    generation_id,
                    destination_digest,
                ));
            }
            WorkerRequest::DispatchExport { plan, response } => {
                let _ = response.send(backend.dispatch_export(plan));
            }
            WorkerRequest::CompleteExport {
                plan,
                outcome,
                response,
            } => {
                let _ = response.send(backend.complete_export(plan, outcome));
            }
            WorkerRequest::GetExportSummary {
                request_id,
                response,
            } => {
                let _ = response.send(backend.export_summary(request_id));
            }
            WorkerRequest::GetReviewResources {
                session_id,
                response,
            } => {
                let result = (|| {
                    let session = backend
                        .get_session(session_id)?
                        .ok_or(PersistenceError::SessionNotFound)?;
                    let generation = backend.active_worktree_generation(&session.workspace_id)?;
                    Ok(ReviewResources {
                        workspace_id: session.workspace_id,
                        generation_id: generation,
                    })
                })();
                let _ = response.send(result);
            }
            WorkerRequest::PrepareRepositoryImport {
                request_id,
                fingerprint,
                source_path_digest,
                session_id,
                response,
            } => {
                let _ = response.send(backend.prepare_repository_import(
                    request_id,
                    fingerprint,
                    source_path_digest,
                    session_id,
                ));
            }
            WorkerRequest::DispatchRepositoryImport { plan, response } => {
                let _ = response.send(backend.dispatch_repository_import(plan));
            }
            WorkerRequest::CompleteRepositoryImport {
                plan,
                outcome,
                response,
            } => {
                let _ = response.send(backend.complete_repository_import(plan, outcome));
            }
            WorkerRequest::RepositoryImportNotApplied { plan, response } => {
                let _ = response.send(backend.mark_repository_import_not_applied(plan));
            }
            WorkerRequest::BlockRepositoryImport { plan, response } => {
                let _ = response.send(backend.block_repository_import(plan));
            }
            WorkerRequest::GetWorkspaceSummary {
                session_id,
                response,
            } => {
                let _ = response.send(backend.workspace_summary(session_id));
            }
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
mod execution_image_tests;
#[cfg(test)]
mod repository_tests;
#[cfg(test)]
mod review_tests;
#[cfg(test)]
mod run_tests;
#[cfg(test)]
mod tests;
