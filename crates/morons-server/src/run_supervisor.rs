use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::{
    sync::{Mutex, OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
    time,
};

use crate::{
    application::events::{AssistantDelta, SessionEventHub},
    persistence::{
        CompletedAssistant, CompletedToolTurn, DispatchOutcome, MAX_TRANSCRIPT_TEXT_BYTES,
        PersistenceError, PrepareOperationOutcome, ProviderOperationFailureState, ProviderUsage,
        RunFailureKind, RunId, RunOpenCodeService, SessionStore, TranscriptEntry,
    },
    provider::{
        OpenCodeProvider, OpenCodeResponseRequest, OpenCodeService, ProviderCancellation,
        ProviderCancellationHandle, ProviderError, ProviderInputItem, ProviderMessagePhase,
        ProviderMessageRole, ProviderOutcome, ProviderOutputItem, ProviderStreamEvent,
        ProviderToolCall, provider_cancellation,
    },
    tools::{
        DirectToolExecutor, TOOL_CATALOG_VERSION, ToolCallValidationError, ToolResult,
        developer_instruction, parse_provider_calls, provider_tools,
    },
};

const MAX_CONCURRENT_RUNS: usize = 4;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RUN_DURATION: Duration = Duration::from_secs(30 * 60);

pub(crate) struct RunSupervisor {
    sessions: Arc<SessionStore>,
    provider: Arc<OpenCodeProvider>,
    permits: Arc<Semaphore>,
    stopping: AtomicBool,
    session_events: Arc<SessionEventHub>,
    state: Mutex<SupervisorState>,
}

struct SupervisorState {
    controls: HashMap<RunId, ProviderCancellationHandle>,
    tasks: JoinSet<()>,
}

impl RunSupervisor {
    pub(crate) fn new(
        sessions: Arc<SessionStore>,
        provider: Arc<OpenCodeProvider>,
        session_events: Arc<SessionEventHub>,
    ) -> Arc<Self> {
        Arc::new(Self {
            sessions,
            provider,
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_RUNS)),
            stopping: AtomicBool::new(false),
            session_events,
            state: Mutex::new(SupervisorState {
                controls: HashMap::new(),
                tasks: JoinSet::new(),
            }),
        })
    }

    pub(crate) fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    pub(crate) fn try_reserve(&self) -> Option<OwnedSemaphorePermit> {
        if self.stopping.load(Ordering::Acquire) {
            return None;
        }
        Arc::clone(&self.permits).try_acquire_owned().ok()
    }

    pub(crate) async fn start(
        self: &Arc<Self>,
        run_id: RunId,
        permit: OwnedSemaphorePermit,
    ) -> Result<(), PersistenceError> {
        let (cancellation_handle, cancellation) = provider_cancellation();
        let mut state = self.state.lock().await;
        while let Some(result) = state.tasks.try_join_next() {
            if let Err(error) = result {
                eprintln!("agent run task failed to join: {error}");
            }
        }
        if state.controls.contains_key(&run_id) {
            return Err(PersistenceError::InvalidState {
                reason: "an accepted run already has a supervisor task",
            });
        }
        if self.stopping.load(Ordering::Acquire) {
            drop(state);
            drop(permit);
            self.sessions.finish_run_stopped(run_id, None).await?;
            return Ok(());
        }
        state.controls.insert(run_id, cancellation_handle);
        let supervisor = Arc::clone(self);
        state.tasks.spawn(async move {
            let _permit = permit;
            if let Err(error) = supervisor.execute_run(run_id, cancellation).await {
                eprintln!("agent run execution failed: {error}");
            }
            supervisor.remove_control(run_id).await;
        });
        Ok(())
    }

    pub(crate) async fn signal_cancellation(&self, run_id: RunId) {
        let state = self.state.lock().await;
        if let Some(handle) = state.controls.get(&run_id) {
            handle.cancel();
        }
    }

    pub(crate) async fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        let mut tasks = {
            let mut state = self.state.lock().await;
            for control in state.controls.values() {
                control.cancel();
            }
            std::mem::replace(&mut state.tasks, JoinSet::new())
        };

        let wait = async {
            while let Some(result) = tasks.join_next().await {
                if let Err(error) = result {
                    eprintln!("agent run task failed during shutdown: {error}");
                }
            }
        };
        if time::timeout(SHUTDOWN_TIMEOUT, wait).await.is_err() {
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        }
        let mut state = self.state.lock().await;
        state.controls.clear();
    }

    async fn execute_run(
        &self,
        run_id: RunId,
        mut cancellation: ProviderCancellation,
    ) -> Result<(), PersistenceError> {
        if self.sessions.activate_run(run_id).await?
            != crate::persistence::ActivationOutcome::Active
        {
            return Ok(());
        }
        let mut delta_sequence = 0_u64;
        let mut reasoning_continuation = None;
        let run_deadline = time::Instant::now() + MAX_RUN_DURATION;
        loop {
            if time::Instant::now() >= run_deadline {
                self.sessions
                    .finish_run_failure(
                        run_id,
                        None,
                        RunFailureKind::ResourceLimit,
                        ProviderOperationFailureState::Failed,
                    )
                    .await?;
                return Ok(());
            }
            let context = self.sessions.load_run_context(run_id).await?;
            let request = match build_provider_request(&context, reasoning_continuation.as_ref()) {
                Ok(request) => request,
                Err(error) => {
                    self.sessions
                        .finish_run_failure(
                            run_id,
                            None,
                            map_provider_failure(error),
                            ProviderOperationFailureState::Failed,
                        )
                        .await?;
                    return Ok(());
                }
            };
            let operation_id = match self
                .sessions
                .prepare_provider_operation(
                    run_id,
                    context.current_entry_high_water,
                    context.estimated_input_tokens,
                )
                .await?
            {
                PrepareOperationOutcome::Prepared(operation_id) => operation_id,
                PrepareOperationOutcome::Cancelled | PrepareOperationOutcome::Terminal => {
                    return Ok(());
                }
            };
            if cancellation.is_cancelled() {
                self.sessions
                    .finish_run_stopped(run_id, Some(operation_id))
                    .await?;
                return Ok(());
            }
            let dispatch = match self
                .provider
                .prepare_dispatch(context.run.credential_generation, &request)
                .await
            {
                Ok(dispatch) => dispatch,
                Err(error) => {
                    self.sessions
                        .finish_run_failure(
                            run_id,
                            Some(operation_id),
                            map_provider_failure(error),
                            ProviderOperationFailureState::Failed,
                        )
                        .await?;
                    return Ok(());
                }
            };
            match self
                .sessions
                .mark_provider_dispatched(run_id, operation_id)
                .await?
            {
                DispatchOutcome::Dispatched => {}
                DispatchOutcome::Cancelled | DispatchOutcome::Terminal => return Ok(()),
            }

            let session_id = context.run.session_id;
            let outcome = time::timeout_at(
                run_deadline,
                dispatch.execute(&mut cancellation, |event| {
                    let ProviderStreamEvent::TextDelta { delta, refusal, .. } = event;
                    if delta.is_empty() {
                        return;
                    }
                    let Some(sequence) = delta_sequence.checked_add(1) else {
                        return;
                    };
                    delta_sequence = sequence;
                    self.session_events.publish_assistant_delta(AssistantDelta {
                        session_id,
                        run_id,
                        sequence,
                        delta,
                        refusal,
                    });
                }),
            )
            .await;
            let outcome = match outcome {
                Err(_) => {
                    self.sessions
                        .finish_run_failure(
                            run_id,
                            Some(operation_id),
                            RunFailureKind::ResourceLimit,
                            ProviderOperationFailureState::Uncertain,
                        )
                        .await?;
                    return Ok(());
                }
                Ok(outcome) => outcome,
            };
            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(ProviderError::Cancelled) => {
                    self.sessions
                        .finish_run_stopped(run_id, Some(operation_id))
                        .await?;
                    return Ok(());
                }
                Err(error) => {
                    self.sessions
                        .finish_run_failure(
                            run_id,
                            Some(operation_id),
                            map_provider_failure(error),
                            provider_failure_state(error),
                        )
                        .await?;
                    return Ok(());
                }
            };
            match normalize_provider_turn(outcome, context.run.tool_catalog_version) {
                Ok(NormalizedTurn::Final(assistant)) => {
                    self.sessions
                        .complete_run_success(run_id, operation_id, assistant)
                        .await?;
                    return Ok(());
                }
                Ok(NormalizedTurn::Tools { turn, reasoning }) => {
                    let committed = match self
                        .sessions
                        .complete_provider_tool_turn(run_id, operation_id, turn)
                        .await
                    {
                        Ok(committed) => committed,
                        Err(PersistenceError::InvalidInput { .. }) => {
                            self.sessions
                                .finish_run_failure(
                                    run_id,
                                    Some(operation_id),
                                    RunFailureKind::InvalidProviderOutput,
                                    ProviderOperationFailureState::Failed,
                                )
                                .await?;
                            return Ok(());
                        }
                        Err(PersistenceError::ResourceLimit { .. }) => {
                            self.sessions
                                .finish_run_failure(
                                    run_id,
                                    Some(operation_id),
                                    RunFailureKind::ResourceLimit,
                                    ProviderOperationFailureState::Failed,
                                )
                                .await?;
                            return Ok(());
                        }
                        Err(error) => return Err(error),
                    };
                    reasoning_continuation =
                        (!reasoning.is_empty()).then_some((*operation_id.as_bytes(), reasoning));
                    let working_directory = context
                        .working_directory
                        .ok_or(PersistenceError::WorkingDirectoryUnavailable)?;
                    let terminal = self
                        .execute_tool_calls(
                            run_id,
                            PathBuf::from(working_directory),
                            committed.calls,
                            &cancellation,
                        )
                        .await?;
                    if terminal {
                        return Ok(());
                    }
                    if cancellation.is_cancelled() {
                        self.sessions.finish_run_stopped(run_id, None).await?;
                        return Ok(());
                    }
                }
                Err(failure) => {
                    self.sessions
                        .finish_run_failure(
                            run_id,
                            Some(operation_id),
                            failure,
                            ProviderOperationFailureState::Failed,
                        )
                        .await?;
                    return Ok(());
                }
            }
        }
    }

    async fn execute_tool_calls(
        &self,
        run_id: RunId,
        working_directory: PathBuf,
        calls: Vec<crate::persistence::CommittedToolCall>,
        cancellation: &ProviderCancellation,
    ) -> Result<bool, PersistenceError> {
        for call in calls {
            self.sessions
                .prepare_tool_operation(run_id, call.call_id, call.operation_id, None)
                .await?;
            if cancellation.is_cancelled() {
                self.sessions
                    .complete_tool_result(
                        run_id,
                        call.call_id,
                        call.operation_id,
                        ToolResult::error(crate::tools::ToolErrorKind::Cancelled),
                    )
                    .await?;
                self.sessions.finish_run_stopped(run_id, None).await?;
                return Ok(true);
            }
            self.sessions
                .mark_tool_dispatched(run_id, call.call_id, call.operation_id)
                .await?;
            let execution_directory = working_directory.clone();
            let execution_input = call.input.clone();
            let execution_cancellation = cancellation.clone();
            let mutation = call.input.kind().is_mutation();
            let result = tokio::task::spawn_blocking(move || {
                DirectToolExecutor::new(execution_directory)
                    .execute(&execution_input, &|| execution_cancellation.is_cancelled())
            })
            .await
            .unwrap_or_else(|_| {
                ToolResult::error(if mutation {
                    crate::tools::ToolErrorKind::Uncertain
                } else {
                    crate::tools::ToolErrorKind::Interrupted
                })
            });
            let cancelled = matches!(
                result,
                ToolResult::Error {
                    error: crate::tools::ToolErrorKind::Cancelled
                }
            );
            let uncertain = result.is_uncertain();
            self.sessions
                .complete_tool_result(run_id, call.call_id, call.operation_id, result)
                .await?;
            if cancelled {
                self.sessions.finish_run_stopped(run_id, None).await?;
                return Ok(true);
            }
            if uncertain {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn remove_control(&self, run_id: RunId) {
        self.state.lock().await.controls.remove(&run_id);
    }
}

fn build_provider_request(
    context: &crate::persistence::RunContext,
    reasoning_continuation: Option<&([u8; 16], Vec<ProviderInputItem>)>,
) -> Result<OpenCodeResponseRequest, ProviderError> {
    let tools_enabled = (
        context.run.tool_catalog_version,
        context.run.tool_limits_version,
    ) == (TOOL_CATALOG_VERSION, crate::tools::TOOL_LIMITS_VERSION);
    if (
        context.run.tool_catalog_version,
        context.run.tool_limits_version,
    ) != (0, 0)
        && !tools_enabled
    {
        return Err(ProviderError::InvalidRequest);
    }
    let mut input = Vec::with_capacity(context.entries.len() + usize::from(tools_enabled));
    if tools_enabled {
        let working_directory = context
            .working_directory
            .as_deref()
            .ok_or(ProviderError::InvalidRequest)?;
        input.push(ProviderInputItem::Message {
            role: ProviderMessageRole::Developer,
            text: format!(
                "{}\nSelected working directory: {working_directory}",
                developer_instruction()
            ),
            phase: None,
        });
    }
    let mut continuation_inserted = false;
    for entry in &context.entries {
        if let (
            TranscriptEntry::ToolCall {
                provider_operation_id,
                ..
            },
            Some((operation_id, reasoning)),
        ) = (entry, reasoning_continuation)
            && provider_operation_id.as_bytes() == operation_id
        {
            input.extend(reasoning.iter().cloned());
            continuation_inserted = true;
        }
        input.push(match entry {
            TranscriptEntry::UserMessage { text, .. } => ProviderInputItem::Message {
                role: ProviderMessageRole::User,
                text: text.clone(),
                phase: None,
            },
            TranscriptEntry::AssistantMessage { text, phase, .. } => ProviderInputItem::Message {
                role: ProviderMessageRole::Assistant,
                text: text.clone(),
                phase: Some(match phase {
                    crate::persistence::AssistantMessagePhase::Commentary => {
                        ProviderMessagePhase::Commentary
                    }
                    crate::persistence::AssistantMessagePhase::Final => {
                        ProviderMessagePhase::FinalAnswer
                    }
                }),
            },
            TranscriptEntry::ToolCall { call_id, input, .. } => ProviderInputItem::FunctionCall {
                call_id: deterministic_provider_call_id(*call_id),
                name: input.kind().name().to_owned(),
                arguments: input
                    .provider_arguments()
                    .map_err(|_| ProviderError::InvalidRequest)?,
            },
            TranscriptEntry::ToolResult {
                call_id, result, ..
            } => ProviderInputItem::FunctionCallOutput {
                call_id: deterministic_provider_call_id(*call_id),
                output: result
                    .provider_output()
                    .map_err(|_| ProviderError::InvalidRequest)?,
            },
        });
    }
    if reasoning_continuation.is_some() && !continuation_inserted {
        return Err(ProviderError::InvalidRequest);
    }
    OpenCodeResponseRequest::new(
        to_provider_service(context.run.service),
        &context.run.model_id,
        context.estimated_input_tokens,
        context.run.maximum_output_tokens,
        input,
        if tools_enabled {
            provider_tools()
        } else {
            Vec::new()
        },
    )
}

fn deterministic_provider_call_id(call_id: crate::persistence::ToolCallId) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(37);
    value.push_str("call_");
    for byte in call_id.as_bytes() {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    value
}

enum NormalizedTurn {
    Final(CompletedAssistant),
    Tools {
        turn: CompletedToolTurn,
        reasoning: Vec<ProviderInputItem>,
    },
}

fn normalize_provider_turn(
    outcome: ProviderOutcome,
    tool_catalog_version: u16,
) -> Result<NormalizedTurn, RunFailureKind> {
    let has_tool_calls = outcome
        .output
        .iter()
        .any(|item| matches!(item, ProviderOutputItem::ToolCall(_)));
    if !has_tool_calls {
        return completed_assistant(outcome).map(NormalizedTurn::Final);
    }
    if tool_catalog_version != TOOL_CATALOG_VERSION {
        return Err(RunFailureKind::InvalidProviderOutput);
    }
    let mut commentary = None;
    let mut calls = Vec::<ProviderToolCall>::new();
    let mut reasoning = Vec::new();
    let mut saw_call = false;
    for item in outcome.output {
        match item {
            ProviderOutputItem::AssistantMessage(message)
                if message.phase == Some(ProviderMessagePhase::Commentary)
                    && !saw_call
                    && !message.text.is_empty()
                    && commentary.is_none() =>
            {
                commentary = Some((message.text, message.refusal));
            }
            ProviderOutputItem::Reasoning(item) => {
                reasoning.push(ProviderInputItem::Reasoning {
                    id: item.provider_item_id,
                    summaries: item.summaries,
                    encrypted_content: item.encrypted_content,
                });
            }
            ProviderOutputItem::ToolCall(call) => {
                saw_call = true;
                calls.push(call);
            }
            ProviderOutputItem::AssistantMessage(_) => {
                return Err(RunFailureKind::InvalidProviderOutput);
            }
        }
    }
    let calls = parse_provider_calls(calls, tool_catalog_version).map_err(|error| match error {
        ToolCallValidationError::InvalidProviderOutput => RunFailureKind::InvalidProviderOutput,
        ToolCallValidationError::ResourceLimit => RunFailureKind::ResourceLimit,
    })?;
    Ok(NormalizedTurn::Tools {
        turn: CompletedToolTurn {
            provider_response_id: outcome.provider_response_id,
            usage: ProviderUsage {
                input_tokens: outcome.usage.input_tokens,
                cached_input_tokens: outcome.usage.cached_input_tokens,
                cache_write_input_tokens: outcome.usage.cache_write_input_tokens,
                output_tokens: outcome.usage.output_tokens,
                reasoning_output_tokens: outcome.usage.reasoning_output_tokens,
                total_tokens: outcome.usage.total_tokens,
            },
            commentary,
            calls,
        },
        reasoning,
    })
}

fn completed_assistant(outcome: ProviderOutcome) -> Result<CompletedAssistant, RunFailureKind> {
    let mut final_message = None;
    for item in outcome.output {
        match item {
            ProviderOutputItem::AssistantMessage(message)
                if message.phase != Some(ProviderMessagePhase::Commentary) =>
            {
                if final_message.replace(message).is_some() {
                    return Err(RunFailureKind::InvalidProviderOutput);
                }
            }
            ProviderOutputItem::AssistantMessage(_) | ProviderOutputItem::Reasoning(_) => {}
            ProviderOutputItem::ToolCall(_) => {
                return Err(RunFailureKind::InvalidProviderOutput);
            }
        }
    }
    let message = final_message.ok_or(RunFailureKind::InvalidProviderOutput)?;
    if message.text.is_empty() {
        return Err(RunFailureKind::InvalidProviderOutput);
    }
    if message.text.len() > MAX_TRANSCRIPT_TEXT_BYTES {
        return Err(RunFailureKind::ResourceLimit);
    }
    Ok(CompletedAssistant {
        text: message.text,
        refusal: message.refusal,
        provider_response_id: outcome.provider_response_id,
        usage: ProviderUsage {
            input_tokens: outcome.usage.input_tokens,
            cached_input_tokens: outcome.usage.cached_input_tokens,
            cache_write_input_tokens: outcome.usage.cache_write_input_tokens,
            output_tokens: outcome.usage.output_tokens,
            reasoning_output_tokens: outcome.usage.reasoning_output_tokens,
            total_tokens: outcome.usage.total_tokens,
        },
    })
}

const fn to_provider_service(service: RunOpenCodeService) -> OpenCodeService {
    match service {
        RunOpenCodeService::Zen => OpenCodeService::Zen,
        RunOpenCodeService::Go => OpenCodeService::Go,
    }
}

const fn map_provider_failure(error: ProviderError) -> RunFailureKind {
    match error {
        ProviderError::CredentialGenerationChanged => RunFailureKind::CredentialChanged,
        ProviderError::CredentialNotConfigured => RunFailureKind::CredentialNotConfigured,
        ProviderError::AuthenticationOrEntitlement => RunFailureKind::AuthenticationOrEntitlement,
        ProviderError::RateLimited => RunFailureKind::RateLimited,
        ProviderError::Unavailable
        | ProviderError::Transport
        | ProviderError::ResponseHeaderTimeout
        | ProviderError::StreamInactivityTimeout
        | ProviderError::TotalTimeout => RunFailureKind::ProviderUnavailable,
        ProviderError::RequestRejected | ProviderError::ProviderExecutionFailed => {
            RunFailureKind::ProviderRejected
        }
        ProviderError::UnexpectedContentType
        | ProviderError::RedirectDenied
        | ProviderError::MalformedResponse
        | ProviderError::IncompleteResponse
        | ProviderError::ResponseLimitExceeded => RunFailureKind::ProviderProtocol,
        ProviderError::InvalidRequest | ProviderError::UnsupportedModel => RunFailureKind::Internal,
        ProviderError::MalformedCatalog | ProviderError::Cancelled => RunFailureKind::Internal,
    }
}

const fn provider_failure_state(error: ProviderError) -> ProviderOperationFailureState {
    match error {
        ProviderError::AuthenticationOrEntitlement
        | ProviderError::RateLimited
        | ProviderError::Unavailable
        | ProviderError::RequestRejected
        | ProviderError::ProviderExecutionFailed => ProviderOperationFailureState::Failed,
        ProviderError::InvalidRequest
        | ProviderError::UnsupportedModel
        | ProviderError::CredentialGenerationChanged
        | ProviderError::CredentialNotConfigured
        | ProviderError::Transport
        | ProviderError::ResponseHeaderTimeout
        | ProviderError::StreamInactivityTimeout
        | ProviderError::TotalTimeout
        | ProviderError::Cancelled
        | ProviderError::RedirectDenied
        | ProviderError::UnexpectedContentType
        | ProviderError::MalformedCatalog
        | ProviderError::MalformedResponse
        | ProviderError::ResponseLimitExceeded
        | ProviderError::IncompleteResponse => ProviderOperationFailureState::Uncertain,
    }
}

#[cfg(test)]
mod tests;
