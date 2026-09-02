use tokio::sync::oneshot;

use super::{
    AcceptedRun, CommittedToolTurn, CompletedToolTurn, MutationRequestId, PersistenceError, Run,
    RunCancellationResult, RunFailureKind, RunId, RunModelSelection, RunOpenCodeService, SessionId,
    SessionStore, ToolCallId, ToolUncertaintyAcknowledgement, TranscriptCursor, TranscriptEntry,
    TranscriptPage, WorkerRequest,
    backend::Backend,
    run_types::{
        self, ActivationOutcome, CompletedAssistant, DispatchOutcome, PrepareOperationOutcome,
        ProviderOperationFailureState, ProviderOperationId, RunContext, ToolOperationId,
    },
    types::{
        REQUEST_FINGERPRINT_BYTES, acknowledge_tool_uncertainty_fingerprint,
        cancel_run_fingerprint, submit_session_input_fingerprint, validate_model_identifier,
        validate_model_selection, validate_user_text,
    },
};
use crate::tools::ToolResult;

impl SessionStore {
    pub async fn find_session_input_retry(
        &self,
        request_id: MutationRequestId,
        session_id: SessionId,
        text: &str,
        service: RunOpenCodeService,
        model_id: &str,
    ) -> Result<Option<AcceptedRun>, PersistenceError> {
        validate_request_id(request_id)?;
        validate_user_text(text)?;
        validate_model_identifier(model_id)?;
        let fingerprint = submit_session_input_fingerprint(session_id, text, service, model_id);
        self.run_request(|response| RunWorkerRequest::FindInputRetry {
            request_id,
            fingerprint,
            response,
        })
        .await
    }

    pub async fn accept_session_input(
        &self,
        request_id: MutationRequestId,
        session_id: SessionId,
        text: String,
        selection: RunModelSelection,
    ) -> Result<AcceptedRun, PersistenceError> {
        validate_request_id(request_id)?;
        validate_user_text(&text)?;
        validate_model_selection(&selection)?;
        let fingerprint = submit_session_input_fingerprint(
            session_id,
            &text,
            selection.service,
            &selection.model_id,
        );
        let _workspace_guard = self.repository_import_lock.lock().await;
        self.run_request(|response| RunWorkerRequest::AcceptInput {
            request_id,
            fingerprint,
            session_id,
            text,
            selection,
            response,
        })
        .await
    }

    pub(crate) async fn activate_run(
        &self,
        run_id: RunId,
    ) -> Result<ActivationOutcome, PersistenceError> {
        self.run_request(|response| RunWorkerRequest::Activate { run_id, response })
            .await
    }

    pub(crate) async fn prepare_provider_operation(
        &self,
        run_id: RunId,
        source_entry_high_water: u64,
        estimated_input_tokens: u32,
    ) -> Result<PrepareOperationOutcome, PersistenceError> {
        self.run_request(|response| RunWorkerRequest::PrepareProvider {
            run_id,
            source_entry_high_water,
            estimated_input_tokens,
            response,
        })
        .await
    }

    pub(crate) async fn mark_provider_dispatched(
        &self,
        run_id: RunId,
        operation_id: ProviderOperationId,
    ) -> Result<DispatchOutcome, PersistenceError> {
        self.run_request(|response| RunWorkerRequest::MarkProviderDispatched {
            run_id,
            operation_id,
            response,
        })
        .await
    }

    pub(crate) async fn complete_provider_tool_turn(
        &self,
        run_id: RunId,
        operation_id: ProviderOperationId,
        turn: CompletedToolTurn,
    ) -> Result<CommittedToolTurn, PersistenceError> {
        self.run_request(|response| RunWorkerRequest::CompleteToolTurn {
            run_id,
            operation_id,
            turn,
            response,
        })
        .await
    }

    pub(crate) async fn prepare_tool_operation(
        &self,
        run_id: RunId,
        call_id: ToolCallId,
        operation_id: ToolOperationId,
        recovery_plan: Option<Vec<u8>>,
    ) -> Result<(), PersistenceError> {
        self.run_request(|response| RunWorkerRequest::PrepareTool {
            run_id,
            call_id,
            operation_id,
            recovery_plan,
            response,
        })
        .await
    }

    pub(crate) async fn mark_tool_dispatched(
        &self,
        run_id: RunId,
        call_id: ToolCallId,
        operation_id: ToolOperationId,
    ) -> Result<(), PersistenceError> {
        self.run_request(|response| RunWorkerRequest::MarkToolDispatched {
            run_id,
            call_id,
            operation_id,
            response,
        })
        .await
    }

    pub(crate) async fn complete_tool_result(
        &self,
        run_id: RunId,
        call_id: ToolCallId,
        operation_id: ToolOperationId,
        result: ToolResult,
    ) -> Result<TranscriptEntry, PersistenceError> {
        self.run_request(|response| RunWorkerRequest::CompleteToolResult {
            run_id,
            call_id,
            operation_id,
            result,
            response,
        })
        .await
    }

    pub(crate) async fn complete_run_success(
        &self,
        run_id: RunId,
        operation_id: ProviderOperationId,
        assistant: CompletedAssistant,
    ) -> Result<Run, PersistenceError> {
        self.run_request(|response| RunWorkerRequest::CompleteSuccess {
            run_id,
            operation_id,
            assistant,
            response,
        })
        .await
    }

    pub(crate) async fn finish_run_failure(
        &self,
        run_id: RunId,
        operation_id: Option<ProviderOperationId>,
        failure: RunFailureKind,
        operation_state: ProviderOperationFailureState,
    ) -> Result<Run, PersistenceError> {
        self.run_request(|response| RunWorkerRequest::FinishFailure {
            run_id,
            operation_id,
            failure,
            operation_state,
            response,
        })
        .await
    }

    pub(crate) async fn finish_run_stopped(
        &self,
        run_id: RunId,
        operation_id: Option<ProviderOperationId>,
    ) -> Result<Run, PersistenceError> {
        self.run_request(|response| RunWorkerRequest::FinishStopped {
            run_id,
            operation_id,
            response,
        })
        .await
    }

    pub async fn cancel_run(
        &self,
        request_id: MutationRequestId,
        session_id: SessionId,
        run_id: RunId,
    ) -> Result<RunCancellationResult, PersistenceError> {
        validate_request_id(request_id)?;
        let fingerprint = cancel_run_fingerprint(session_id, run_id);
        self.run_request(|response| RunWorkerRequest::Cancel {
            request_id,
            fingerprint,
            session_id,
            run_id,
            response,
        })
        .await
    }

    pub async fn acknowledge_tool_uncertainty(
        &self,
        request_id: MutationRequestId,
        session_id: SessionId,
        run_id: RunId,
    ) -> Result<ToolUncertaintyAcknowledgement, PersistenceError> {
        validate_request_id(request_id)?;
        let fingerprint = acknowledge_tool_uncertainty_fingerprint(session_id, run_id);
        self.run_request(|response| RunWorkerRequest::AcknowledgeToolUncertainty {
            request_id,
            fingerprint,
            session_id,
            run_id,
            response,
        })
        .await
    }

    pub async fn get_run(
        &self,
        session_id: SessionId,
        run_id: RunId,
    ) -> Result<Option<Run>, PersistenceError> {
        self.run_request(|response| RunWorkerRequest::Get {
            session_id,
            run_id,
            response,
        })
        .await
    }

    pub async fn list_session_transcript(
        &self,
        session_id: SessionId,
        cursor: Option<TranscriptCursor>,
        limit: u16,
    ) -> Result<TranscriptPage, PersistenceError> {
        if limit == 0 || limit > run_types::MAX_TRANSCRIPT_PAGE_SIZE {
            return Err(PersistenceError::InvalidInput {
                reason: "a transcript page size must be exactly 1",
            });
        }
        self.run_request(|response| RunWorkerRequest::ListTranscript {
            session_id,
            cursor,
            limit,
            response,
        })
        .await
    }

    pub(crate) async fn load_run_context(
        &self,
        run_id: RunId,
    ) -> Result<RunContext, PersistenceError> {
        self.run_request(|response| RunWorkerRequest::LoadContext { run_id, response })
            .await
    }

    async fn run_request<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T, PersistenceError>>) -> RunWorkerRequest,
    ) -> Result<T, PersistenceError> {
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::Run(build(response_sender)))
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }
}

fn validate_request_id(request_id: MutationRequestId) -> Result<(), PersistenceError> {
    if request_id.is_zero() {
        return Err(PersistenceError::InvalidInput {
            reason: "a mutation request identifier must not be all zeroes",
        });
    }
    Ok(())
}

pub(super) enum RunWorkerRequest {
    FindInputRetry {
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        response: oneshot::Sender<Result<Option<AcceptedRun>, PersistenceError>>,
    },
    AcceptInput {
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        session_id: SessionId,
        text: String,
        selection: RunModelSelection,
        response: oneshot::Sender<Result<AcceptedRun, PersistenceError>>,
    },
    Activate {
        run_id: RunId,
        response: oneshot::Sender<Result<ActivationOutcome, PersistenceError>>,
    },
    PrepareProvider {
        run_id: RunId,
        source_entry_high_water: u64,
        estimated_input_tokens: u32,
        response: oneshot::Sender<Result<PrepareOperationOutcome, PersistenceError>>,
    },
    MarkProviderDispatched {
        run_id: RunId,
        operation_id: ProviderOperationId,
        response: oneshot::Sender<Result<DispatchOutcome, PersistenceError>>,
    },
    CompleteToolTurn {
        run_id: RunId,
        operation_id: ProviderOperationId,
        turn: CompletedToolTurn,
        response: oneshot::Sender<Result<CommittedToolTurn, PersistenceError>>,
    },
    PrepareTool {
        run_id: RunId,
        call_id: ToolCallId,
        operation_id: ToolOperationId,
        recovery_plan: Option<Vec<u8>>,
        response: oneshot::Sender<Result<(), PersistenceError>>,
    },
    MarkToolDispatched {
        run_id: RunId,
        call_id: ToolCallId,
        operation_id: ToolOperationId,
        response: oneshot::Sender<Result<(), PersistenceError>>,
    },
    CompleteToolResult {
        run_id: RunId,
        call_id: ToolCallId,
        operation_id: ToolOperationId,
        result: ToolResult,
        response: oneshot::Sender<Result<TranscriptEntry, PersistenceError>>,
    },
    CompleteSuccess {
        run_id: RunId,
        operation_id: ProviderOperationId,
        assistant: CompletedAssistant,
        response: oneshot::Sender<Result<Run, PersistenceError>>,
    },
    FinishFailure {
        run_id: RunId,
        operation_id: Option<ProviderOperationId>,
        failure: RunFailureKind,
        operation_state: ProviderOperationFailureState,
        response: oneshot::Sender<Result<Run, PersistenceError>>,
    },
    FinishStopped {
        run_id: RunId,
        operation_id: Option<ProviderOperationId>,
        response: oneshot::Sender<Result<Run, PersistenceError>>,
    },
    Cancel {
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        session_id: SessionId,
        run_id: RunId,
        response: oneshot::Sender<Result<RunCancellationResult, PersistenceError>>,
    },
    AcknowledgeToolUncertainty {
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        session_id: SessionId,
        run_id: RunId,
        response: oneshot::Sender<Result<ToolUncertaintyAcknowledgement, PersistenceError>>,
    },
    Get {
        session_id: SessionId,
        run_id: RunId,
        response: oneshot::Sender<Result<Option<Run>, PersistenceError>>,
    },
    ListTranscript {
        session_id: SessionId,
        cursor: Option<TranscriptCursor>,
        limit: u16,
        response: oneshot::Sender<Result<TranscriptPage, PersistenceError>>,
    },
    LoadContext {
        run_id: RunId,
        response: oneshot::Sender<Result<RunContext, PersistenceError>>,
    },
}

impl RunWorkerRequest {
    pub(super) fn execute(self, backend: &mut Backend) {
        match self {
            Self::FindInputRetry {
                request_id,
                fingerprint,
                response,
            } => {
                let _ = response.send(backend.find_run_input_retry(request_id, fingerprint));
            }
            Self::AcceptInput {
                request_id,
                fingerprint,
                session_id,
                text,
                selection,
                response,
            } => {
                let _ = response.send(backend.accept_session_input(
                    request_id,
                    fingerprint,
                    session_id,
                    text,
                    selection,
                ));
            }
            Self::Activate { run_id, response } => {
                let _ = response.send(backend.activate_run(run_id));
            }
            Self::PrepareProvider {
                run_id,
                source_entry_high_water,
                estimated_input_tokens,
                response,
            } => {
                let _ = response.send(backend.prepare_provider_operation(
                    run_id,
                    source_entry_high_water,
                    estimated_input_tokens,
                ));
            }
            Self::MarkProviderDispatched {
                run_id,
                operation_id,
                response,
            } => {
                let _ = response.send(backend.mark_provider_dispatched(run_id, operation_id));
            }
            Self::CompleteToolTurn {
                run_id,
                operation_id,
                turn,
                response,
            } => {
                let _ =
                    response.send(backend.complete_provider_tool_turn(run_id, operation_id, turn));
            }
            Self::PrepareTool {
                run_id,
                call_id,
                operation_id,
                recovery_plan,
                response,
            } => {
                let _ = response.send(backend.prepare_tool_operation(
                    run_id,
                    call_id,
                    operation_id,
                    recovery_plan,
                ));
            }
            Self::MarkToolDispatched {
                run_id,
                call_id,
                operation_id,
                response,
            } => {
                let _ = response.send(backend.mark_tool_dispatched(run_id, call_id, operation_id));
            }
            Self::CompleteToolResult {
                run_id,
                call_id,
                operation_id,
                result,
                response,
            } => {
                let _ = response.send(backend.complete_tool_result(
                    run_id,
                    call_id,
                    operation_id,
                    result,
                ));
            }
            Self::CompleteSuccess {
                run_id,
                operation_id,
                assistant,
                response,
            } => {
                let _ =
                    response.send(backend.complete_run_success(run_id, operation_id, assistant));
            }
            Self::FinishFailure {
                run_id,
                operation_id,
                failure,
                operation_state,
                response,
            } => {
                let _ = response.send(backend.finish_run_failure(
                    run_id,
                    operation_id,
                    failure,
                    operation_state,
                ));
            }
            Self::FinishStopped {
                run_id,
                operation_id,
                response,
            } => {
                let _ = response.send(backend.finish_run_stopped(run_id, operation_id));
            }
            Self::Cancel {
                request_id,
                fingerprint,
                session_id,
                run_id,
                response,
            } => {
                let _ =
                    response.send(backend.cancel_run(request_id, fingerprint, session_id, run_id));
            }
            Self::AcknowledgeToolUncertainty {
                request_id,
                fingerprint,
                session_id,
                run_id,
                response,
            } => {
                let _ = response.send(backend.acknowledge_tool_uncertainty(
                    request_id,
                    fingerprint,
                    session_id,
                    run_id,
                ));
            }
            Self::Get {
                session_id,
                run_id,
                response,
            } => {
                let _ = response.send(backend.get_run(session_id, run_id));
            }
            Self::ListTranscript {
                session_id,
                cursor,
                limit,
                response,
            } => {
                let _ = response.send(backend.list_session_transcript(session_id, cursor, limit));
            }
            Self::LoadContext { run_id, response } => {
                let _ = response.send(backend.load_run_context(run_id));
            }
        }
    }
}
