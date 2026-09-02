use std::sync::Arc;

use tokio::sync::oneshot;

use super::{
    ExecutionImageOutcome, ExecutionImagePlan, ExecutionImageSummary, MutationRequestId,
    PersistenceError, SessionStore, WorkerRequest,
    execution_image::ExecutionImageRecovery,
    types::{
        REQUEST_FINGERPRINT_BYTES, execution_image_source_path_digest,
        provision_execution_image_fingerprint, validate_execution_image_source_path,
    },
};

impl SessionStore {
    pub async fn execution_image_summary(&self) -> Result<ExecutionImageSummary, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::GetExecutionImageSummary { response })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub async fn provision_execution_image(
        self: &Arc<Self>,
        request_id: MutationRequestId,
        toolchain_source_path: String,
        cargo_source_path: String,
    ) -> Result<ExecutionImageSummary, PersistenceError> {
        if request_id.is_zero() {
            return Err(PersistenceError::InvalidInput {
                reason: "a mutation request identifier must not be all zeroes",
            });
        }
        validate_execution_image_source_path(&toolchain_source_path)?;
        validate_execution_image_source_path(&cargo_source_path)?;
        let toolchain_digest = execution_image_source_path_digest(1, &toolchain_source_path);
        let cargo_digest = execution_image_source_path_digest(2, &cargo_source_path);
        let fingerprint = provision_execution_image_fingerprint(toolchain_digest, cargo_digest);
        let store = Arc::clone(self);
        tokio::spawn(async move {
            store
                .provision_execution_image_owned(
                    request_id,
                    fingerprint,
                    toolchain_digest,
                    cargo_digest,
                    toolchain_source_path,
                    cargo_source_path,
                )
                .await
        })
        .await
        .map_err(|_| PersistenceError::WorkerStopped)?
    }

    #[allow(clippy::too_many_arguments)]
    async fn provision_execution_image_owned(
        &self,
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        toolchain_digest: [u8; REQUEST_FINGERPRINT_BYTES],
        cargo_digest: [u8; REQUEST_FINGERPRINT_BYTES],
        toolchain_source_path: String,
        cargo_source_path: String,
    ) -> Result<ExecutionImageSummary, PersistenceError> {
        let _guard = self.execution_image_lock.lock().await;
        let prepared = self
            .prepare_execution_image(request_id, fingerprint, toolchain_digest, cargo_digest)
            .await?;
        match prepared.state {
            2 => return self.execution_image_summary().await,
            3 => return Err(PersistenceError::ExecutionImageProvisionNotApplied),
            4 => return Err(PersistenceError::ExecutionImageBlocked),
            0 | 1 => {}
            _ => {
                return Err(PersistenceError::InvalidState {
                    reason: "an execution image request has an unknown state",
                });
            }
        }
        let newly_dispatched = prepared.state == 0;
        let plan = if newly_dispatched {
            self.dispatch_execution_image(prepared).await?
        } else {
            prepared
        };
        let paths = self.paths.clone();
        let effect = if newly_dispatched {
            tokio::task::spawn_blocking(move || {
                paths.provision_execution_image(plan, &toolchain_source_path, &cargo_source_path)
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
            .map(ExecutionImageRecovery::Complete)
        } else {
            tokio::task::spawn_blocking(move || paths.recover_execution_image(plan))
                .await
                .map_err(|_| PersistenceError::WorkerStopped)?
        };
        match effect {
            Ok(ExecutionImageRecovery::Complete(outcome)) => {
                let summary = self.complete_execution_image(plan, outcome).await?;
                let paths = self.paths.clone();
                tokio::task::spawn_blocking(move || {
                    paths.cleanup_inactive_execution_images(Some(plan.generation_id))
                })
                .await
                .map_err(|_| PersistenceError::WorkerStopped)??;
                Ok(summary)
            }
            Ok(ExecutionImageRecovery::NotApplied) => {
                self.mark_execution_image_not_applied(plan).await?;
                Err(PersistenceError::ExecutionImageProvisionNotApplied)
            }
            Ok(ExecutionImageRecovery::Blocked) | Err(PersistenceError::ExecutionImageBlocked) => {
                self.block_execution_image(plan).await?;
                Err(PersistenceError::ExecutionImageBlocked)
            }
            Err(error) => {
                self.mark_execution_image_not_applied(plan).await?;
                Err(error)
            }
        }
    }

    async fn prepare_execution_image(
        &self,
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        toolchain_source_digest: [u8; REQUEST_FINGERPRINT_BYTES],
        cargo_source_digest: [u8; REQUEST_FINGERPRINT_BYTES],
    ) -> Result<ExecutionImagePlan, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::PrepareExecutionImage {
                request_id,
                fingerprint,
                toolchain_source_digest,
                cargo_source_digest,
                response,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    async fn dispatch_execution_image(
        &self,
        plan: ExecutionImagePlan,
    ) -> Result<ExecutionImagePlan, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::DispatchExecutionImage { plan, response })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    async fn complete_execution_image(
        &self,
        plan: ExecutionImagePlan,
        outcome: ExecutionImageOutcome,
    ) -> Result<ExecutionImageSummary, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::CompleteExecutionImage {
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

    async fn mark_execution_image_not_applied(
        &self,
        plan: ExecutionImagePlan,
    ) -> Result<ExecutionImageSummary, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::ExecutionImageNotApplied { plan, response })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    async fn block_execution_image(
        &self,
        plan: ExecutionImagePlan,
    ) -> Result<ExecutionImageSummary, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::BlockExecutionImage { plan, response })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }
}
