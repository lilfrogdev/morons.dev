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
    pub absence_generation: Option<u64>,
    pub exa_contract_revision: u16,
    pub policy_sequence: u64,
    pub sequence: u64,
    pub query_digest: Option<[u8; 32]>,
    pub children: u16,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum WebRoute {
    OpenAi,
    ExaMissingCredential,
    ExaPolicyDenied,
}
impl WebRoute {
    pub(crate) fn record(self) -> i64 {
        match self {
            Self::OpenAi => 0,
            Self::ExaMissingCredential => 1,
            Self::ExaPolicyDenied => 2,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct WebInvocation {
    pub child: u16,
    pub ordinal: u64,
    pub query_digest: [u8; 32],
    pub route: WebRoute,
}

pub(super) enum Request {
    Complete {
        binding: WebBinding,
        invocation: WebInvocation,
        result: crate::tools::ToolResult,
        response: oneshot::Sender<Result<crate::tools::WebSuccess, PersistenceError>>,
    },
    Dispatch {
        binding: WebBinding,
        invocation: WebInvocation,
        response: oneshot::Sender<Result<DataUsePolicy, PersistenceError>>,
    },
    Load {
        run: RunId,
        call: ToolCallId,
        response: oneshot::Sender<Result<WebBinding, PersistenceError>>,
    },
    Admit {
        binding: WebBinding,
        exa: bool,
        response: oneshot::Sender<Result<DataUsePolicy, PersistenceError>>,
    },
}
impl Request {
    pub(super) fn execute(self, backend: &mut Backend) {
        match self {
            Self::Complete {
                binding,
                invocation,
                result,
                response,
            } => {
                let _ = response.send(backend.complete_web_search(&binding, &invocation, &result));
            }
            Self::Dispatch {
                binding,
                invocation,
                response,
            } => {
                let _ = response.send(backend.dispatch_web_search(&binding, &invocation));
            }
            Self::Load {
                run,
                call,
                response,
            } => {
                let _ = response.send(backend.load_web_binding(run, call));
            }
            Self::Admit {
                binding,
                exa,
                response,
            } => {
                let _ = response.send(backend.admit_web_route(&binding, exa));
            }
        }
    }
}
impl SessionStore {
    pub(crate) async fn complete_web_search(
        &self,
        binding: &WebBinding,
        invocation: WebInvocation,
        result: crate::tools::ToolResult,
    ) -> Result<crate::tools::WebSuccess, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::WebBinding(Request::Complete {
                binding: binding.clone(),
                invocation,
                result,
                response,
            }))
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub(crate) async fn dispatch_web_search(
        &self,
        binding: &WebBinding,
        invocation: WebInvocation,
    ) -> Result<DataUsePolicy, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::WebBinding(Request::Dispatch {
                binding: binding.clone(),
                invocation,
                response,
            }))
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

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
        self.admit_web_route(binding, false).await
    }
    pub(crate) async fn admit_web_route(
        &self,
        binding: &WebBinding,
        exa: bool,
    ) -> Result<DataUsePolicy, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::WebBinding(Request::Admit {
                binding: binding.clone(),
                exa,
                response,
            }))
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }
}
