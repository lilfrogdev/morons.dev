use std::sync::Arc;

use tokio::sync::oneshot;

use super::{
    MutationRequestId, PersistenceError, RepositoryImportOutcome, RepositoryImportPlan, SessionId,
    SessionStore, WorkerRequest, WorkspaceSummary,
    types::{
        REQUEST_FINGERPRINT_BYTES, import_repository_fingerprint, repository_source_path_digest,
        validate_repository_source_path,
    },
    workspace::RepositoryRecovery,
};

impl SessionStore {
    pub async fn import_repository(
        self: &Arc<Self>,
        request_id: MutationRequestId,
        session_id: SessionId,
        source_path: String,
    ) -> Result<WorkspaceSummary, PersistenceError> {
        if request_id.is_zero() {
            return Err(PersistenceError::InvalidInput {
                reason: "a mutation request identifier must not be all zeroes",
            });
        }
        validate_repository_source_path(&source_path)?;
        let fingerprint = import_repository_fingerprint(session_id, &source_path);
        let source_path_digest = repository_source_path_digest(&source_path);
        let store = Arc::clone(self);
        tokio::spawn(async move {
            store
                .import_repository_owned(
                    request_id,
                    fingerprint,
                    source_path_digest,
                    session_id,
                    source_path,
                )
                .await
        })
        .await
        .map_err(|_| PersistenceError::WorkerStopped)?
    }

    async fn import_repository_owned(
        &self,
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        source_path_digest: [u8; REQUEST_FINGERPRINT_BYTES],
        session_id: SessionId,
        source_path: String,
    ) -> Result<WorkspaceSummary, PersistenceError> {
        let _guard = self.repository_import_lock.lock().await;
        let prepared = self
            .prepare_repository_import(request_id, fingerprint, source_path_digest, session_id)
            .await?;
        match prepared.state {
            2 => return self.workspace_summary(session_id).await,
            3 => return Err(PersistenceError::RepositoryImportNotApplied),
            4 => return Err(PersistenceError::WorkspaceBlocked),
            0 | 1 => {}
            _ => {
                return Err(PersistenceError::InvalidState {
                    reason: "a repository import has an unknown state",
                });
            }
        }
        let newly_dispatched = prepared.state == 0;
        let plan = if newly_dispatched {
            self.dispatch_repository_import(prepared).await?
        } else {
            prepared
        };
        let paths = self.paths.clone();
        let effect = if newly_dispatched {
            tokio::task::spawn_blocking(move || paths.import_repository(plan, &source_path))
                .await
                .map_err(|_| PersistenceError::WorkerStopped)?
                .map(RepositoryRecovery::Complete)
        } else {
            tokio::task::spawn_blocking(move || paths.recover_repository_import(plan))
                .await
                .map_err(|_| PersistenceError::WorkerStopped)?
        };
        match effect {
            Ok(RepositoryRecovery::Complete(outcome)) => {
                self.complete_repository_import(plan, outcome).await
            }
            Ok(RepositoryRecovery::NotApplied) => {
                self.mark_repository_import_not_applied(plan).await?;
                Err(PersistenceError::RepositoryImportNotApplied)
            }
            Ok(RepositoryRecovery::Blocked) | Err(PersistenceError::WorkspaceBlocked) => {
                self.block_repository_import(plan).await?;
                Err(PersistenceError::WorkspaceBlocked)
            }
            Err(error) => {
                self.mark_repository_import_not_applied(plan).await?;
                Err(error)
            }
        }
    }

    pub(crate) async fn drain_workspace_operations(&self) {
        let _guard = self.repository_import_lock.lock().await;
    }

    async fn prepare_repository_import(
        &self,
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        source_path_digest: [u8; REQUEST_FINGERPRINT_BYTES],
        session_id: SessionId,
    ) -> Result<RepositoryImportPlan, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::PrepareRepositoryImport {
                request_id,
                fingerprint,
                source_path_digest,
                session_id,
                response,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    async fn dispatch_repository_import(
        &self,
        plan: RepositoryImportPlan,
    ) -> Result<RepositoryImportPlan, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::DispatchRepositoryImport { plan, response })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    async fn complete_repository_import(
        &self,
        plan: RepositoryImportPlan,
        outcome: RepositoryImportOutcome,
    ) -> Result<WorkspaceSummary, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::CompleteRepositoryImport {
                plan,
                outcome,
                response,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    async fn mark_repository_import_not_applied(
        &self,
        plan: RepositoryImportPlan,
    ) -> Result<WorkspaceSummary, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::RepositoryImportNotApplied { plan, response })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    async fn block_repository_import(
        &self,
        plan: RepositoryImportPlan,
    ) -> Result<WorkspaceSummary, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::BlockRepositoryImport { plan, response })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub(super) async fn workspace_summary(
        &self,
        session_id: SessionId,
    ) -> Result<WorkspaceSummary, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::GetWorkspaceSummary {
                session_id,
                response,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }
}
