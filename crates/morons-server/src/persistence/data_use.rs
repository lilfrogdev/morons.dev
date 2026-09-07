#[cfg(test)]
pub(crate) mod tests;

use tokio::sync::oneshot;

use super::{
    MutationRequestId, PersistenceError, RunOpenCodeService, SessionStore, WorkerRequest,
    backend::Backend,
};
use crate::provider::DataUseRestrictions;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DataUsePolicy {
    pub sequence: u64,
    pub restrictions: DataUseRestrictions,
}

pub(super) enum Request {
    Get(oneshot::Sender<Result<DataUsePolicy, PersistenceError>>),
    Set {
        id: MutationRequestId,
        expected_sequence: u64,
        restrictions: DataUseRestrictions,
        response: oneshot::Sender<Result<DataUsePolicy, PersistenceError>>,
    },
    Admit {
        service: RunOpenCodeService,
        model: String,
        response: oneshot::Sender<Result<DataUsePolicy, PersistenceError>>,
    },
}

impl Request {
    pub(super) fn execute(self, backend: &mut Backend) {
        match self {
            Self::Get(response) => {
                let _ = response.send(backend.data_use_policy());
            }
            Self::Set {
                id,
                expected_sequence,
                restrictions,
                response,
            } => {
                let _ =
                    response.send(backend.set_data_use_policy(id, expected_sequence, restrictions));
            }
            Self::Admit {
                service,
                model,
                response,
            } => {
                let _ = response.send(backend.admit_model_data_use(service, &model));
            }
        }
    }
}

impl SessionStore {
    pub async fn data_use_policy(&self) -> Result<DataUsePolicy, PersistenceError> {
        self.data_use_request(Request::Get).await
    }

    pub async fn set_data_use_policy(
        &self,
        id: MutationRequestId,
        expected_sequence: u64,
        restrictions: DataUseRestrictions,
    ) -> Result<DataUsePolicy, PersistenceError> {
        if id.is_zero() || expected_sequence > i64::MAX as u64 {
            return Err(PersistenceError::InvalidInput {
                reason: "data-use mutation identity or sequence is invalid",
            });
        }
        self.data_use_request(|response| Request::Set {
            id,
            expected_sequence,
            restrictions,
            response,
        })
        .await
    }

    pub(crate) async fn admit_model_data_use(
        &self,
        service: RunOpenCodeService,
        model: &str,
    ) -> Result<DataUsePolicy, PersistenceError> {
        super::types::validate_model_identifier(model)?;
        self.data_use_request(|response| Request::Admit {
            service,
            model: model.to_owned(),
            response,
        })
        .await
    }

    async fn data_use_request(
        &self,
        request: impl FnOnce(oneshot::Sender<Result<DataUsePolicy, PersistenceError>>) -> Request,
    ) -> Result<DataUsePolicy, PersistenceError> {
        let (sender, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::DataUse(request(sender)))
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }
}
