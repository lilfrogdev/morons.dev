use super::{PersistenceError, RunId, RunService, SessionStore, ToolCallId, WorkerRequest};
use tokio::sync::oneshot;

#[derive(Clone, Debug)]
pub(crate) struct TaskModelBinding {
    pub run_id: RunId,
    pub call_id: ToolCallId,
    pub operation_id: [u8; 16],
    pub service: RunService,
    pub model_id: String,
    pub credential_generation: u64,
    pub protocol_revision: u16,
    pub maximum_input_tokens: u32,
    pub maximum_output_tokens: u32,
    pub policy_sequence: u64,
    pub sequence: u64,
}
impl SessionStore {
    pub(crate) async fn task_model_binding(
        &self,
        run_id: RunId,
        call_id: ToolCallId,
    ) -> Result<TaskModelBinding, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::TaskModelBinding {
                run_id,
                call_id,
                response,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }
}
