use tokio::sync::oneshot;

use super::{
    AcceptedLocalCommand, LocalCommandCancellationResult, LocalCommandId, MutationRequestId,
    PersistenceError, SessionId, SessionStore, WorkerRequest,
    backend::{Backend, local_command},
};
use crate::tools::ToolResult;

impl SessionStore {
    pub async fn find_local_command_retry(
        &self,
        request_id: MutationRequestId,
        session_id: SessionId,
        command: &str,
        context_visible: bool,
    ) -> Result<Option<AcceptedLocalCommand>, PersistenceError> {
        validate_request_id(request_id)?;
        local_command::validate_local_command(command)?;
        let fingerprint =
            local_command::local_command_fingerprint(session_id, command, context_visible);
        self.local_command_request(|response| LocalCommandWorkerRequest::FindRetry {
            request_id,
            fingerprint,
            session_id,
            command: command.to_owned(),
            context_visible,
            response,
        })
        .await
    }

    pub async fn accept_local_command(
        &self,
        request_id: MutationRequestId,
        session_id: SessionId,
        command: String,
        context_visible: bool,
    ) -> Result<AcceptedLocalCommand, PersistenceError> {
        validate_request_id(request_id)?;
        local_command::validate_local_command(&command)?;
        let fingerprint =
            local_command::local_command_fingerprint(session_id, &command, context_visible);
        self.local_command_request(|response| LocalCommandWorkerRequest::Accept {
            request_id,
            fingerprint,
            session_id,
            command,
            context_visible,
            response,
        })
        .await
    }

    pub(crate) async fn activate_local_command(
        &self,
        command_id: LocalCommandId,
    ) -> Result<bool, PersistenceError> {
        self.local_command_request(|response| LocalCommandWorkerRequest::Activate {
            command_id,
            response,
        })
        .await
    }

    pub(crate) async fn complete_local_command(
        &self,
        command_id: LocalCommandId,
        result: ToolResult,
    ) -> Result<super::TranscriptEntry, PersistenceError> {
        self.local_command_request(|response| LocalCommandWorkerRequest::Complete {
            command_id,
            result,
            response,
        })
        .await
    }

    pub async fn cancel_local_command(
        &self,
        request_id: MutationRequestId,
        session_id: SessionId,
        command_id: LocalCommandId,
    ) -> Result<LocalCommandCancellationResult, PersistenceError> {
        validate_request_id(request_id)?;
        let fingerprint = local_command::cancel_local_command_fingerprint(session_id, command_id);
        self.local_command_request(|response| LocalCommandWorkerRequest::Cancel {
            request_id,
            fingerprint,
            session_id,
            command_id,
            response,
        })
        .await
    }

    async fn local_command_request<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T, PersistenceError>>) -> LocalCommandWorkerRequest,
    ) -> Result<T, PersistenceError> {
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::LocalCommand(build(response_sender)))
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }
}

fn validate_request_id(request_id: MutationRequestId) -> Result<(), PersistenceError> {
    if request_id.is_zero() {
        Err(PersistenceError::InvalidInput {
            reason: "a mutation request identifier must not be all zeroes",
        })
    } else {
        Ok(())
    }
}

pub(super) enum LocalCommandWorkerRequest {
    FindRetry {
        request_id: MutationRequestId,
        fingerprint: [u8; 32],
        session_id: SessionId,
        command: String,
        context_visible: bool,
        response: oneshot::Sender<Result<Option<AcceptedLocalCommand>, PersistenceError>>,
    },
    Accept {
        request_id: MutationRequestId,
        fingerprint: [u8; 32],
        session_id: SessionId,
        command: String,
        context_visible: bool,
        response: oneshot::Sender<Result<AcceptedLocalCommand, PersistenceError>>,
    },
    Activate {
        command_id: LocalCommandId,
        response: oneshot::Sender<Result<bool, PersistenceError>>,
    },
    Complete {
        command_id: LocalCommandId,
        result: ToolResult,
        response: oneshot::Sender<Result<super::TranscriptEntry, PersistenceError>>,
    },
    Cancel {
        request_id: MutationRequestId,
        fingerprint: [u8; 32],
        session_id: SessionId,
        command_id: LocalCommandId,
        response: oneshot::Sender<Result<LocalCommandCancellationResult, PersistenceError>>,
    },
}

impl LocalCommandWorkerRequest {
    pub(super) fn execute(self, backend: &mut Backend) {
        match self {
            Self::FindRetry {
                request_id,
                fingerprint,
                session_id,
                command,
                context_visible,
                response,
            } => {
                let _ = response.send(backend.find_local_command_retry(
                    request_id,
                    fingerprint,
                    session_id,
                    &command,
                    context_visible,
                ));
            }
            Self::Accept {
                request_id,
                fingerprint,
                session_id,
                command,
                context_visible,
                response,
            } => {
                let _ = response.send(backend.accept_local_command(
                    request_id,
                    fingerprint,
                    session_id,
                    command,
                    context_visible,
                ));
            }
            Self::Activate {
                command_id,
                response,
            } => {
                let _ = response.send(backend.activate_local_command(command_id));
            }
            Self::Complete {
                command_id,
                result,
                response,
            } => {
                let _ = response.send(backend.complete_local_command(command_id, result));
            }
            Self::Cancel {
                request_id,
                fingerprint,
                session_id,
                command_id,
                response,
            } => {
                let _ = response.send(backend.cancel_local_command(
                    request_id,
                    fingerprint,
                    session_id,
                    command_id,
                ));
            }
        }
    }
}
