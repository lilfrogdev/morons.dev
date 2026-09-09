use super::{
    DataUsePolicy, PersistenceError, RunId, SessionStore, ToolCallId, WorkerRequest,
    backend::Backend,
};
use tokio::sync::oneshot;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WebBinding {
    pub run_id: RunId,
    pub call_id: ToolCallId,
    pub operation_id: [u8; 16],
    pub generation: u64,
    pub policy_sequence: u64,
    pub sequence: u64,
    pub query_digest: Option<[u8; 32]>,
    pub children: u16,
}

pub(super) enum Request {
    Load {
        run: RunId,
        call: ToolCallId,
        response: oneshot::Sender<Result<WebBinding, PersistenceError>>,
    },
    Admit {
        binding: WebBinding,
        response: oneshot::Sender<Result<DataUsePolicy, PersistenceError>>,
    },
}
impl Request {
    pub(super) fn execute(self, backend: &mut Backend) {
        match self {
            Self::Load {
                run,
                call,
                response,
            } => {
                let _ = response.send(backend.load_web_binding(run, call));
            }
            Self::Admit { binding, response } => {
                let _ = response.send(backend.admit_web_binding(&binding));
            }
        }
    }
}
impl SessionStore {
    pub(crate) async fn web_binding(
        &self,
        run: RunId,
        call: ToolCallId,
    ) -> Result<WebBinding, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::WebBinding(Request::Load {
                run,
                call,
                response,
            }))
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }
    pub(crate) async fn admit_web_binding(
        &self,
        binding: &WebBinding,
    ) -> Result<DataUsePolicy, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::WebBinding(Request::Admit {
                binding: binding.clone(),
                response,
            }))
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }
}
