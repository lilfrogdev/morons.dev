use super::{
    ExportPlan, MutationRequestId, PersistenceError, RepositoryImportOutcome, ReviewResources,
    SessionId, SessionStore, WorkerRequest,
};
use morons_protocol::{DiffChange, DiffCursor, ExportSummary, ReviewGeneration};
use sha2::{Digest, Sha256};
use std::{
    path::{Component, Path},
    sync::Arc,
};
use tokio::sync::oneshot;

impl SessionStore {
    pub async fn review_diff(
        &self,
        session_id: SessionId,
        cursor: Option<DiffCursor>,
        limit: u16,
    ) -> Result<(Vec<DiffChange>, Option<DiffCursor>, ReviewGeneration), PersistenceError> {
        if limit == 0 || limit > 50 {
            return Err(PersistenceError::InvalidInput {
                reason: "diff page limit is invalid",
            });
        }
        let resources = self.review_resources(session_id).await?;
        let workspace = resources.workspace_id;
        let generation = resources.generation_id;
        let token = token(session_id, generation);
        let after = match cursor {
            Some(cursor) => {
                if cursor.generation != token {
                    return Err(PersistenceError::ReviewCursorStale);
                }
                cursor.after_path
            }
            None => None,
        };
        let paths = self.paths.clone();
        let after_copy = after.clone();
        let changes = tokio::task::spawn_blocking(move || {
            paths.review_diff(&workspace, &generation, after_copy.as_deref(), limit)
        })
        .await
        .map_err(|_| PersistenceError::WorkerStopped)??;
        let next = if changes.len() == usize::from(limit) {
            changes.last().map(|change| DiffCursor {
                generation: token,
                after_path: Some(change.path.clone()),
            })
        } else {
            None
        };
        Ok((changes, next, token))
    }
    pub async fn export_worktree(
        self: &Arc<Self>,
        request_id: MutationRequestId,
        session_id: SessionId,
        generation: ReviewGeneration,
        destination: String,
    ) -> Result<ExportSummary, PersistenceError> {
        validate_destination(&destination)?;
        let bytes = generation.as_bytes();
        if bytes[..16] != session_id.as_bytes()[..] {
            return Err(PersistenceError::ReviewCursorStale);
        }
        let generation_id = bytes[16..]
            .try_into()
            .map_err(|_| PersistenceError::ReviewCursorStale)?;
        let digest = destination_digest(&destination);
        let _guard = self.repository_import_lock.lock().await;
        let plan = self
            .prepare_export(request_id, session_id, generation_id, digest)
            .await?;
        match plan.state {
            2 => return self.export_summary(request_id).await,
            3 => return Err(PersistenceError::ExportNotApplied),
            1 | 4 => return Err(PersistenceError::ExportUncertain),
            0 => {}
            _ => {
                return Err(PersistenceError::InvalidState {
                    reason: "export state is invalid",
                });
            }
        }
        let plan = self.dispatch_export(plan).await?;
        let paths = self.paths.clone();
        let target = destination;
        let outcome = tokio::task::spawn_blocking(move || {
            paths.export_worktree(
                &plan.workspace_id,
                &plan.generation_id,
                &plan.operation_id,
                &target,
            )
        })
        .await
        .map_err(|_| PersistenceError::WorkerStopped)??;
        self.complete_export(plan, outcome).await
    }
    async fn review_resources(
        &self,
        session_id: SessionId,
    ) -> Result<ReviewResources, PersistenceError> {
        let (s, r) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::GetReviewResources {
                session_id,
                response: s,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        r.await.map_err(|_| PersistenceError::WorkerStopped)?
    }
    async fn prepare_export(
        &self,
        request_id: MutationRequestId,
        session_id: SessionId,
        generation_id: [u8; 16],
        destination_digest: [u8; 32],
    ) -> Result<ExportPlan, PersistenceError> {
        let (s, r) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::PrepareExport {
                request_id,
                session_id,
                generation_id,
                destination_digest,
                response: s,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        r.await.map_err(|_| PersistenceError::WorkerStopped)?
    }
    async fn dispatch_export(&self, plan: ExportPlan) -> Result<ExportPlan, PersistenceError> {
        let (s, r) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::DispatchExport { plan, response: s })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        r.await.map_err(|_| PersistenceError::WorkerStopped)?
    }
    async fn complete_export(
        &self,
        plan: ExportPlan,
        outcome: RepositoryImportOutcome,
    ) -> Result<ExportSummary, PersistenceError> {
        let (s, r) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::CompleteExport {
                plan,
                outcome,
                response: s,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        r.await.map_err(|_| PersistenceError::WorkerStopped)?
    }
    async fn export_summary(
        &self,
        request_id: MutationRequestId,
    ) -> Result<ExportSummary, PersistenceError> {
        let (s, r) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::GetExportSummary {
                request_id,
                response: s,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        r.await.map_err(|_| PersistenceError::WorkerStopped)?
    }
}
fn token(session: SessionId, generation: [u8; 16]) -> ReviewGeneration {
    let mut bytes = [0; 32];
    bytes[..16].copy_from_slice(session.as_bytes());
    bytes[16..].copy_from_slice(&generation);
    ReviewGeneration::from_bytes(bytes)
}
fn destination_digest(path: &str) -> [u8; 32] {
    let mut d = Sha256::new();
    d.update(b"morons.dev/export-destination/v1\0");
    d.update((path.len() as u32).to_be_bytes());
    d.update(path.as_bytes());
    d.finalize().into()
}
fn validate_destination(value: &str) -> Result<(), PersistenceError> {
    if value.is_empty()
        || value.len() > morons_protocol::MAX_EXPORT_DESTINATION_PATH_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(PersistenceError::InvalidInput {
            reason: "export destination is invalid",
        });
    }
    let path = Path::new(value);
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
    {
        return Err(PersistenceError::InvalidInput {
            reason: "export destination must be normalized and absolute",
        });
    }
    Ok(())
}
