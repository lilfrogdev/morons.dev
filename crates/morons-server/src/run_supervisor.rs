use crate::debug_log::DebugNormalizationStage;

mod subagent;

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
    sync::{Mutex, OwnedSemaphorePermit, Semaphore, watch},
    task::JoinSet,
    time,
};

use self::subagent::SubagentExecutor;
#[cfg(test)]
use crate::provider::OpenCodeResponseRequest;
use crate::provider::dispatch::{ModelInput, ModelProviders};
use crate::{
    application::events::{AssistantDelta, NativeResponseDiagnostic, SessionEventHub},
    persistence::{
        CompletedAssistant, CompletedToolTurn, DispatchOutcome, MAX_TRANSCRIPT_TEXT_BYTES,
        PersistenceError, PrepareOperationOutcome, ProviderOperationFailureState, ProviderUsage,
        RunFailureKind, RunId, SessionStore, ToolCallId, TranscriptEntry,
    },
    provider::{
        OpenCodeProvider, ProviderCancellation, ProviderCancellationHandle, ProviderContentPart,
        ProviderError, ProviderInputItem, ProviderMessagePhase, ProviderMessageRole,
        ProviderOutcome, ProviderOutputItem, ProviderStreamEvent, ProviderToolCall,
        provider_cancellation,
    },
    tools::{
        BashToolExecutor, DirectToolExecutor, IpythonSupervisor, TOOL_CATALOG_VERSION,
        ToolCallValidationError, ToolKind, ToolResult, ValidatedProviderCall,
        WebSearchToolExecutor, developer_instruction, parse_provider_calls_diagnosed,
        parse_subagent_provider_calls_diagnosed, provider_tools,
    },
};

const MAX_CONCURRENT_RUNS: usize = 4;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RUN_DURATION: Duration = Duration::from_secs(30 * 60);

struct ProviderTurnContinuation {
    reasoning: Option<([u8; 16], Vec<ProviderInputItem>)>,
    tool_calls: Vec<(ToolCallId, String)>,
}

pub(crate) struct RunSupervisor {
    sessions: Arc<SessionStore>,
    provider: Arc<ModelProviders>,
    maintenance: Arc<crate::maintenance_supervisor::MaintenanceSupervisor>,
    permits: Arc<Semaphore>,
    stopping: AtomicBool,
    shutdown_requests: watch::Sender<bool>,
    session_events: Arc<SessionEventHub>,
    web_search: Arc<WebSearchToolExecutor>,
    ipython: Arc<IpythonSupervisor>,
    subagents: SubagentExecutor,
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
        let managed_python_root = sessions.managed_python_root();
        Self::with_tools(
            sessions.clone(),
            provider,
            session_events,
            WebSearchToolExecutor::new(sessions),
            IpythonSupervisor::new(managed_python_root),
        )
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        sessions: Arc<SessionStore>,
        provider: Arc<OpenCodeProvider>,
        session_events: Arc<SessionEventHub>,
        search_origin: String,
    ) -> Arc<Self> {
        Self::with_tools(
            sessions.clone(),
            provider,
            session_events,
            WebSearchToolExecutor::for_test(sessions, search_origin),
            IpythonSupervisor::for_test(),
        )
    }

    #[cfg(test)]
    pub(crate) fn with_ipython_for_test(
        sessions: Arc<SessionStore>,
        provider: Arc<OpenCodeProvider>,
        session_events: Arc<SessionEventHub>,
    ) -> Arc<Self> {
        Self::with_tools(
            sessions.clone(),
            provider,
            session_events,
            WebSearchToolExecutor::for_test(sessions, "http://127.0.0.1:9/search".to_owned()),
            IpythonSupervisor::for_test(),
        )
    }

    fn with_tools(
        sessions: Arc<SessionStore>,
        provider: Arc<OpenCodeProvider>,
        session_events: Arc<SessionEventHub>,
        web_search: WebSearchToolExecutor,
        ipython: Arc<IpythonSupervisor>,
    ) -> Arc<Self> {
        Self::with_model_providers(
            sessions.clone(),
            ModelProviders::new(sessions, provider),
            session_events,
            web_search,
            ipython,
        )
    }

    pub(crate) fn providers(&self) -> Arc<ModelProviders> {
        self.provider.clone()
    }

    pub(crate) fn with_model_providers(
        sessions: Arc<SessionStore>,
        provider: Arc<ModelProviders>,
        session_events: Arc<SessionEventHub>,
        web_search: WebSearchToolExecutor,
        ipython: Arc<IpythonSupervisor>,
    ) -> Arc<Self> {
        let web_search = Arc::new(web_search);
        let subagents = SubagentExecutor::new(
            Arc::clone(&sessions),
            Arc::clone(&provider),
            Arc::clone(&web_search),
        );
        let shutdown_requests = watch::channel(false).0;
        let maintenance = crate::maintenance_supervisor::MaintenanceSupervisor::new(
            Arc::clone(&sessions),
            Arc::clone(&provider),
            shutdown_requests.clone(),
        );
        Arc::new(Self {
            maintenance,
            sessions,
            provider,
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_RUNS)),
            stopping: AtomicBool::new(false),
            shutdown_requests,
            session_events,
            web_search,
            ipython,
            subagents,
            state: Mutex::new(SupervisorState {
                controls: HashMap::new(),
                tasks: JoinSet::new(),
            }),
        })
    }

    pub(crate) fn shutdown_requests(&self) -> watch::Sender<bool> {
        self.shutdown_requests.clone()
    }

    pub(crate) fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire) || *self.shutdown_requests.borrow()
    }

    pub(crate) fn try_reserve(&self) -> Option<OwnedSemaphorePermit> {
        if self.is_stopping() {
            return None;
        }
        Arc::clone(&self.permits).try_acquire_owned().ok()
    }

    pub(crate) async fn start(
        self: &Arc<Self>,
        run_id: RunId,
        session_id: crate::persistence::SessionId,
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
        if self.is_stopping() {
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
                // No success/terminal event can be claimed if persistence failed.
                // Stop admission and let orderly shutdown/restart recover uncertain facts.
                eprintln!("agent run persistence failed; requesting server shutdown: {error}");
                supervisor.stopping.store(true, Ordering::Release);
                supervisor.shutdown_requests.send_replace(true);
            }
            supervisor.remove_control(run_id).await;
            if !supervisor.is_stopping() {
                supervisor.maintenance.maybe_start(session_id, run_id).await;
            }
        });
        Ok(())
    }

    pub(crate) async fn signal_cancellation(&self, run_id: RunId) {
        let state = self.state.lock().await;
        if let Some(handle) = state.controls.get(&run_id) {
            handle.cancel();
        }
    }

    pub(crate) async fn terminate_session_runtime(
        &self,
        session_id: crate::persistence::SessionId,
    ) -> bool {
        if self.maintenance.cancel_session(session_id).await.is_err() {
            return false;
        }
        self.ipython.terminate_session(session_id).await
    }

    pub(crate) async fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        self.maintenance.shutdown().await;
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
        drop(state);
        self.ipython.shutdown().await;
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
        let mut provider_continuation = None;
        let mut provider_turn = None;
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
                crate::debug_log::emit(crate::debug_log::DebugEvent::Resource {
                    location: crate::debug_log::DebugLocation::Root {
                        run_id: *run_id.as_bytes(),
                    },
                    resource: crate::debug_log::DebugResource::RootDeadline,
                });
                return Ok(());
            }
            if cancellation.is_cancelled() {
                self.sessions.finish_run_stopped(run_id, None).await?;
                return Ok(());
            }
            if let Some(session) = self.sessions.maintenance_boundary(run_id).await? {
                self.maintenance.cancel_session(session).await?;
            }
            let mut context = match self.sessions.load_run_context(run_id).await {
                Ok(context) => context,
                Err(error) => return self.fail_between_turns(run_id, error).await,
            };
            if let Some(plan) = context.compaction_plan.take() {
                match self
                    .execute_compaction(run_id, &context, plan, &mut cancellation)
                    .await?
                {
                    Ok(()) => {
                        provider_continuation = None;
                        provider_turn = None;
                        continue;
                    }
                    Err(ProviderError::Cancelled) => {
                        self.sessions.finish_run_stopped(run_id, None).await?;
                        return Ok(());
                    }
                    Err(error) => {
                        self.sessions
                            .finish_run_failure(
                                run_id,
                                None,
                                self.classify_provider_failure(error),
                                if error == ProviderError::DataUseRestricted {
                                    ProviderOperationFailureState::Failed
                                } else {
                                    ProviderOperationFailureState::Uncertain
                                },
                            )
                            .await?;
                        return Ok(());
                    }
                }
            }
            let request = match (|| {
                if provider_turn.is_none() {
                    provider_turn = Some(self.provider.turn(
                        context.run.service.model_service(),
                        &context.run.model_id,
                        *context.run.session_id.as_bytes(),
                        *run_id.as_bytes(),
                        context.run.credential_generation,
                    )?);
                }
                provider_turn
                    .as_ref()
                    .ok_or(ProviderError::InvalidRequest)?
                    .request(build_provider_input(
                        &context,
                        provider_continuation.as_ref(),
                    )?)
            })() {
                Ok(request) => request,
                Err(error) => {
                    self.sessions
                        .finish_run_failure(
                            run_id,
                            None,
                            self.classify_provider_failure(error),
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
                .await
            {
                Err(error) => return self.fail_between_turns(run_id, error).await,
                Ok(PrepareOperationOutcome::Prepared(operation_id)) => operation_id,
                Ok(PrepareOperationOutcome::Cancelled | PrepareOperationOutcome::Terminal) => {
                    return Ok(());
                }
            };
            if cancellation.is_cancelled() {
                self.sessions
                    .finish_run_stopped(run_id, Some(operation_id))
                    .await?;
                return Ok(());
            }
            let policy = self.sessions.data_use_policy().await?.restrictions;
            let dispatch = match self
                .provider
                .prepare_dispatch(
                    provider_turn
                        .as_mut()
                        .expect("a prepared request has a turn"),
                    &request,
                    policy,
                    &mut cancellation,
                )
                .await
            {
                Ok(dispatch) => dispatch,
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
                            self.classify_provider_failure(error),
                            ProviderOperationFailureState::Failed,
                        )
                        .await?;
                    return Ok(());
                }
            };
            match self
                .sessions
                .mark_provider_dispatched(run_id, operation_id)
                .await
            {
                Ok(DispatchOutcome::Dispatched) => {}
                Ok(DispatchOutcome::Cancelled | DispatchOutcome::Terminal) => return Ok(()),
                Err(PersistenceError::DataUseRestricted) => {
                    self.sessions
                        .finish_run_failure(
                            run_id,
                            Some(operation_id),
                            RunFailureKind::DataUseRestricted,
                            ProviderOperationFailureState::Failed,
                        )
                        .await?;
                    return Ok(());
                }
                Err(error) => return Err(error),
            }

            let session_id = context.run.session_id;
            let outcome = time::timeout_at(
                run_deadline,
                dispatch.execute(policy, &mut cancellation, |event| {
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
                    crate::debug_log::emit(crate::debug_log::DebugEvent::Resource {
                        location: crate::debug_log::DebugLocation::Root {
                            run_id: *run_id.as_bytes(),
                        },
                        resource: crate::debug_log::DebugResource::RootDeadline,
                    });
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
                            self.classify_provider_failure(error),
                            provider_failure_state(error),
                        )
                        .await?;
                    // Only after the durable failure commit, never as assistant output.
                    if let Some(reason) = provider_turn
                        .as_ref()
                        .and_then(|turn| turn.native_response_failure())
                    {
                        self.session_events
                            .publish_native_diagnostic(NativeResponseDiagnostic {
                                session_id,
                                run_id,
                                reason,
                            });
                    }
                    return Ok(());
                }
            };
            let mut stage = DebugNormalizationStage::Other;
            match normalize_provider_turn_diagnosed(
                outcome,
                context.run.tool_catalog_version,
                &mut stage,
            ) {
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
                        Err(PersistenceError::ResourceLimit { resource }) => {
                            self.sessions
                                .finish_run_failure(
                                    run_id,
                                    Some(operation_id),
                                    RunFailureKind::ResourceLimit,
                                    ProviderOperationFailureState::Failed,
                                )
                                .await?;
                            crate::debug_log::emit(crate::debug_log::DebugEvent::Resource {
                                location: crate::debug_log::DebugLocation::Root {
                                    run_id: *run_id.as_bytes(),
                                },
                                resource: resource.into(),
                            });
                            return Ok(());
                        }
                        Err(error) => return Err(error),
                    };
                    let mut tool_calls = provider_continuation
                        .take()
                        .map(|continuation| continuation.tool_calls)
                        .unwrap_or_default();
                    tool_calls.extend(committed.calls.iter().filter_map(|call| {
                        call.opaque_continuation
                            .as_ref()
                            .map(|continuation| (call.call_id, continuation.clone()))
                    }));
                    let reasoning =
                        (!reasoning.is_empty()).then_some((*operation_id.as_bytes(), reasoning));
                    provider_continuation = (reasoning.is_some() || !tool_calls.is_empty())
                        .then_some(ProviderTurnContinuation {
                            reasoning,
                            tool_calls,
                        });
                    let working_directory = context
                        .working_directory
                        .as_ref()
                        .ok_or(PersistenceError::WorkingDirectoryUnavailable)?;
                    let terminal = self
                        .execute_tool_calls(
                            &context,
                            PathBuf::from(working_directory),
                            committed.calls,
                            crate::provider::find_model_profile(
                                context.run.service.model_service(),
                                &context.run.model_id,
                            )
                            .is_some_and(|model| model.capabilities.image_input),
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
                    crate::debug_log::emit(crate::debug_log::DebugEvent::Normalization {
                        location: crate::debug_log::DebugLocation::Root {
                            run_id: *run_id.as_bytes(),
                        },
                        stage,
                        resource_limit: failure == RunFailureKind::ResourceLimit,
                    });
                    return Ok(());
                }
            }
        }
    }

    fn classify_provider_failure(&self, error: ProviderError) -> RunFailureKind {
        if error == ProviderError::CredentialStoreUnavailable {
            self.shutdown_requests.send_replace(true);
        }
        map_provider_failure(error)
    }

    // Called only between completed tool/provider turns, with no effect in flight.
    async fn fail_between_turns(
        &self,
        run_id: RunId,
        error: PersistenceError,
    ) -> Result<(), PersistenceError> {
        let resource = match &error {
            PersistenceError::ResourceLimit { resource } => Some((*resource).into()),
            _ => None,
        };
        let failure = match error {
            PersistenceError::ResourceLimit { .. } => RunFailureKind::ResourceLimit,
            PersistenceError::DataUseRestricted => RunFailureKind::DataUseRestricted,
            PersistenceError::WorkingDirectoryUnavailable => RunFailureKind::ToolExecution,
            other => return Err(other),
        };
        self.sessions
            .finish_run_failure(run_id, None, failure, ProviderOperationFailureState::Failed)
            .await?;
        if let Some(resource) = resource {
            crate::debug_log::emit(crate::debug_log::DebugEvent::Resource {
                location: crate::debug_log::DebugLocation::Root {
                    run_id: *run_id.as_bytes(),
                },
                resource,
            });
        }
        Ok(())
    }

    async fn execute_compaction(
        &self,
        run_id: RunId,
        context: &crate::persistence::RunContext,
        plan: crate::persistence::CompactionPlan,
        cancellation: &mut ProviderCancellation,
    ) -> Result<Result<(), ProviderError>, PersistenceError> {
        self.maintenance
            .cancel_session(context.run.session_id)
            .await?;
        let operation_id = self.sessions.prepare_auto_compaction(run_id, &plan).await?;
        let built = (|| {
            let conversation =
                if context.run.service == crate::persistence::RunService::OpenAiChatGpt {
                    *operation_id.as_bytes()
                } else {
                    *context.run.session_id.as_bytes()
                };
            let turn = self.provider.turn(
                context.run.service.model_service(),
                &context.run.model_id,
                conversation,
                *run_id.as_bytes(),
                context.run.credential_generation,
            )?;
            let request = turn.request(build_compaction_input(&context.run, &plan)?)?;
            Ok((turn, request))
        })();
        let (mut turn, request) = match built {
            Ok(prepared) => prepared,
            Err(error) => {
                self.sessions
                    .fail_compaction(run_id, operation_id, false)
                    .await?;
                return Ok(Err(error));
            }
        };
        let policy = self.sessions.data_use_policy().await?.restrictions;
        let dispatch = match self
            .provider
            .prepare_dispatch(&mut turn, &request, policy, cancellation)
            .await
        {
            Ok(dispatch) => dispatch,
            Err(error) => {
                self.sessions
                    .fail_compaction(run_id, operation_id, false)
                    .await?;
                return Ok(Err(error));
            }
        };
        match self
            .sessions
            .mark_compaction_dispatched(run_id, operation_id)
            .await
        {
            Ok(()) => {}
            Err(PersistenceError::DataUseRestricted) => {
                self.sessions
                    .fail_compaction(run_id, operation_id, false)
                    .await?;
                return Ok(Err(ProviderError::DataUseRestricted));
            }
            Err(error) => return Err(error),
        }
        let outcome = dispatch.execute(policy, cancellation, |_| {}).await;
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                self.sessions
                    .fail_compaction(run_id, operation_id, true)
                    .await?;
                return Ok(Err(error));
            }
        };
        let assistant = match completed_assistant(outcome) {
            Ok(assistant) => assistant,
            Err(failure) => {
                self.sessions
                    .fail_compaction(run_id, operation_id, true)
                    .await?;
                return Ok(Err(match failure {
                    RunFailureKind::ResourceLimit => ProviderError::ResponseLimitExceeded,
                    _ => ProviderError::MalformedResponse,
                }));
            }
        };
        match self
            .sessions
            .complete_compaction(
                run_id,
                operation_id,
                context.run.service,
                context.run.model_id.clone(),
                assistant.text,
            )
            .await
        {
            Ok(_) => Ok(Ok(())),
            Err(PersistenceError::ResourceLimit { .. }) => {
                self.sessions
                    .fail_compaction(run_id, operation_id, true)
                    .await?;
                Ok(Err(ProviderError::ResponseLimitExceeded))
            }
            Err(error) => Err(error),
        }
    }

    async fn execute_tool_calls(
        &self,
        context: &crate::persistence::RunContext,
        working_directory: PathBuf,
        calls: Vec<crate::persistence::CommittedToolCall>,
        supports_image_input: bool,
        cancellation: &ProviderCancellation,
    ) -> Result<bool, PersistenceError> {
        let run = &context.run;
        let run_id = run.id;
        let session_id = run.session_id;
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
            let tool = call.input.kind();
            match self
                .sessions
                .mark_tool_dispatched(run_id, call.call_id, call.operation_id)
                .await
            {
                Ok(()) => {}
                Err(
                    PersistenceError::CredentialNotConfigured
                    | PersistenceError::OpenAiCredentialNotConfigured
                    | PersistenceError::CredentialReauthenticationRequired
                    | PersistenceError::InvalidInput { .. },
                ) if matches!(tool, ToolKind::Task | ToolKind::WebSearch) => {
                    self.sessions
                        .complete_tool_result(
                            run_id,
                            call.call_id,
                            call.operation_id,
                            ToolResult::error(if tool == ToolKind::WebSearch {
                                crate::tools::ToolErrorKind::CredentialNotConfigured
                            } else {
                                crate::tools::ToolErrorKind::ModelUnavailable
                            }),
                        )
                        .await?;
                    continue;
                }
                Err(error) => return Err(error),
            }
            let task_binding = if tool == ToolKind::Task {
                Some(
                    self.sessions
                        .task_model_binding(run_id, call.call_id)
                        .await?,
                )
            } else {
                None
            };
            let execution_directory = working_directory.clone();
            let execution_input = call.input.clone();
            let execution_cancellation = cancellation.clone();
            let mutation = tool.is_mutation();
            let result = if tool == ToolKind::Task {
                self.subagents
                    .execute(
                        context,
                        call.call_id,
                        execution_directory,
                        &execution_input,
                        task_binding.expect("dispatched task tools have a durable model binding"),
                        &execution_cancellation,
                    )
                    .await?
            } else if tool == ToolKind::WebSearch {
                let binding = self.sessions.web_binding(run_id, call.call_id).await?;
                self.web_search
                    .execute(&execution_input, &binding, 0, 0, &execution_cancellation)
                    .await?
            } else if tool == ToolKind::Ipython {
                self.ipython
                    .execute(
                        session_id,
                        execution_directory,
                        &execution_input,
                        &execution_cancellation,
                    )
                    .await
            } else {
                tokio::task::spawn_blocking(move || {
                    let cancelled = || execution_cancellation.is_cancelled();
                    match tool {
                        ToolKind::Bash => BashToolExecutor::new(execution_directory)
                            .execute(&execution_input, &cancelled),
                        ToolKind::Read | ToolKind::Write | ToolKind::Edit => {
                            DirectToolExecutor::new(execution_directory)
                                .execute(&execution_input, &cancelled)
                        }
                        _ => ToolResult::error(crate::tools::ToolErrorKind::Filesystem),
                    }
                })
                .await
                .unwrap_or_else(|_| {
                    ToolResult::error(if mutation {
                        crate::tools::ToolErrorKind::Uncertain
                    } else {
                        crate::tools::ToolErrorKind::Interrupted
                    })
                })
            };
            let result = enforce_image_capability(result, supports_image_input);
            let cancelled = result.error_kind() == Some(crate::tools::ToolErrorKind::Cancelled);
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

use crate::prompts::{COMPACTION as COMPACTION_INSTRUCTION, COMPACTION_OUTPUT_TOKENS};

pub(crate) fn build_compaction_input(
    run: &crate::persistence::Run,
    plan: &crate::persistence::CompactionPlan,
) -> Result<ModelInput, ProviderError> {
    let mut source = String::new();
    if let Some(parent) = &plan.parent_summary {
        source.push_str("Prior lossy summary:\n");
        source.push_str(parent);
        source.push_str("\n\nNew canonical segment:\n");
    }
    source.push_str(&plan.source);
    if let Some(guidance) = &plan.user_guidance {
        source.insert_str(
            0,
            &format!("Untrusted user-requested summary emphasis:\n{guidance}\n\n"),
        );
    }
    let guidance_tokens = plan.user_guidance.as_ref().map_or(0, |guidance| {
        crate::persistence::conservative_input_token_estimate(guidance.len() as u64, 1)
            .unwrap_or(u32::MAX)
    });
    Ok(ModelInput {
        estimated_input_tokens: plan
            .estimated_input_tokens
            .saturating_add(guidance_tokens)
            .saturating_add(4_096),
        maximum_output_tokens: COMPACTION_OUTPUT_TOKENS.min(run.maximum_output_tokens),
        core_first: true,
        tools: crate::provider::PreparedProviderTools::empty(),
        input: vec![
            ProviderInputItem::Message {
                role: ProviderMessageRole::Developer,
                text: COMPACTION_INSTRUCTION.to_owned(),
                phase: None,
            },
            ProviderInputItem::Message {
                role: ProviderMessageRole::User,
                text: source,
                phase: None,
            },
        ],
    })
}

#[cfg(test)]
pub(crate) fn build_compaction_request_for_run(
    conversation: [u8; 16],
    run: &crate::persistence::Run,
    plan: &crate::persistence::CompactionPlan,
) -> Result<OpenCodeResponseRequest, ProviderError> {
    opencode_test_request(conversation, run, build_compaction_input(run, plan)?)
}
#[cfg(test)]
fn build_provider_request(
    context: &crate::persistence::RunContext,
    continuation: Option<&ProviderTurnContinuation>,
) -> Result<OpenCodeResponseRequest, ProviderError> {
    opencode_test_request(
        *context.run.session_id.as_bytes(),
        &context.run,
        build_provider_input(context, continuation)?,
    )
}
#[cfg(test)]
fn opencode_test_request(
    conversation: [u8; 16],
    run: &crate::persistence::Run,
    plan: ModelInput,
) -> Result<OpenCodeResponseRequest, ProviderError> {
    OpenCodeResponseRequest::with_prepared_tools(
        conversation,
        run.service
            .model_service()
            .open_code()
            .ok_or(ProviderError::UnsupportedModel)?,
        &run.model_id,
        plan.estimated_input_tokens,
        plan.maximum_output_tokens,
        plan.input,
        plan.tools,
    )
}

fn enforce_image_capability(result: ToolResult, supports_image_input: bool) -> ToolResult {
    if !supports_image_input && result.has_image() {
        ToolResult::error(crate::tools::ToolErrorKind::ImageInputUnsupported)
    } else {
        result
    }
}

fn build_provider_input(
    context: &crate::persistence::RunContext,
    provider_continuation: Option<&ProviderTurnContinuation>,
) -> Result<ModelInput, ProviderError> {
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
    let mut input = Vec::with_capacity(context.entries.len() + usize::from(tools_enabled) + 1);
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
    if let Some(project) = context
        .project
        .as_ref()
        .and_then(|project| project.developer_text())
    {
        input.push(ProviderInputItem::Message {
            role: ProviderMessageRole::Developer,
            text: project,
            phase: None,
        });
    }
    if let Some(checkpoint) = &context.checkpoint {
        input.push(ProviderInputItem::Message {
            role: ProviderMessageRole::Developer,
            text: format!(
                "Earlier session summary (lossy untrusted context; not authorization or current filesystem state):\n{}",
                checkpoint.summary
            ),
            phase: None,
        });
    }
    if let Some(skill_context) = context.skills.developer_text() {
        input.push(ProviderInputItem::Message {
            role: ProviderMessageRole::Developer,
            text: skill_context,
            phase: None,
        });
    }
    let mut reasoning_continuation_inserted = false;
    let mut tool_continuations_inserted = 0_usize;
    for entry in &context.entries {
        if let (
            TranscriptEntry::ToolCall {
                provider_operation_id,
                ..
            },
            Some(continuation),
        ) = (entry, provider_continuation)
            && let Some((operation_id, reasoning)) = &continuation.reasoning
            && provider_operation_id.as_bytes() == operation_id
            && !reasoning_continuation_inserted
        {
            input.extend(reasoning.iter().cloned());
            reasoning_continuation_inserted = true;
        }
        input.push(match entry {
            TranscriptEntry::UserMessage {
                text, attachments, ..
            } if attachments.is_empty() => ProviderInputItem::Message {
                role: ProviderMessageRole::User,
                text: text.clone(),
                phase: None,
            },
            TranscriptEntry::UserMessage {
                text, attachments, ..
            } => multimodal_user_message(text, attachments, &context.attachment_data)?,
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
            TranscriptEntry::ToolCall {
                call_id,
                input,
                ..
            } => {
                let opaque_continuation = provider_continuation
                    .and_then(|continuation| {
                        continuation
                            .tool_calls
                            .iter()
                            .find(|(continuation_call_id, _)| continuation_call_id == call_id)
                    })
                    .map(|(_, continuation)| continuation.clone());
                if opaque_continuation.is_some() {
                    tool_continuations_inserted = tool_continuations_inserted
                        .checked_add(1)
                        .ok_or(ProviderError::InvalidRequest)?;
                }
                ProviderInputItem::FunctionCall {
                    call_id: deterministic_provider_call_id(*call_id),
                    name: input.kind().name().to_owned(),
                    arguments: input
                        .provider_arguments()
                        .map_err(|_| ProviderError::InvalidRequest)?,
                    opaque_continuation,
                }
            }
            TranscriptEntry::ToolResult {
                call_id, result, ..
            } => ProviderInputItem::FunctionCallOutput {
                call_id: deterministic_provider_call_id(*call_id),
                output: result
                    .provider_output()
                    .map_err(|_| ProviderError::InvalidRequest)?,
            },
            TranscriptEntry::LocalCommand {
                command,
                status,
                exit_code,
                signal,
                stdout,
                stderr,
                context_visible: true,
                ..
            } => ProviderInputItem::Message {
                role: ProviderMessageRole::User,
                text: format!(
                    "Local command (status: {status:?}, exit_code: {exit_code:?}, signal: {signal:?}):\n{command}\nstdout:\n{stdout}\nstderr:\n{stderr}"
                ),
                phase: None,
            },
            TranscriptEntry::LocalCommand {
                context_visible: false,
                ..
            } => return Err(ProviderError::InvalidRequest),
        });
        if let TranscriptEntry::ToolResult {
            result:
                ToolResult::Ok {
                    output: crate::tools::ToolOutput::ReadImage { image, .. },
                },
            ..
        } = entry
        {
            input.push(tool_image_message(image, &context.attachment_data)?);
        }
    }
    if provider_continuation.is_some_and(|continuation| {
        (continuation.reasoning.is_some() && !reasoning_continuation_inserted)
            || continuation.tool_calls.len() != tool_continuations_inserted
    }) {
        return Err(ProviderError::InvalidRequest);
    }
    Ok(ModelInput {
        estimated_input_tokens: context.estimated_input_tokens,
        maximum_output_tokens: context.run.maximum_output_tokens,
        input,
        core_first: tools_enabled,
        tools: if tools_enabled {
            provider_tools()?
        } else {
            crate::provider::PreparedProviderTools::empty()
        },
    })
}

fn multimodal_user_message(
    text: &str,
    attachments: &[crate::persistence::ImageAttachment],
    data: &std::collections::HashMap<crate::persistence::ImageAttachmentId, Vec<u8>>,
) -> Result<ProviderInputItem, ProviderError> {
    let mut parts = Vec::with_capacity(attachments.len() * 2 + 1);
    let mut cursor = 0_usize;
    for attachment in attachments {
        let start =
            usize::try_from(attachment.marker_start).map_err(|_| ProviderError::InvalidRequest)?;
        let marker_end = start
            .checked_add(attachment.display_name.len() + 2)
            .ok_or(ProviderError::InvalidRequest)?;
        let text_part = text
            .get(cursor..marker_end)
            .filter(|part| !part.is_empty())
            .ok_or(ProviderError::InvalidRequest)?;
        parts.push(ProviderContentPart::Text(text_part.to_owned()));
        let bytes = data
            .get(&attachment.id)
            .filter(|bytes| bytes.len() as u64 == attachment.bytes)
            .ok_or(ProviderError::InvalidRequest)?;
        parts.push(ProviderContentPart::Image {
            media_type: attachment.media_type,
            width: attachment.width,
            height: attachment.height,
            bytes: bytes.clone(),
        });
        cursor = marker_end;
    }
    if let Some(remainder) = text.get(cursor..).filter(|part| !part.is_empty()) {
        parts.push(ProviderContentPart::Text(remainder.to_owned()));
    }
    Ok(ProviderInputItem::MultimodalMessage {
        role: ProviderMessageRole::User,
        parts,
        phase: None,
    })
}

fn tool_image_message(
    image: &crate::tools::ToolImageOutput,
    data: &std::collections::HashMap<crate::persistence::ImageAttachmentId, Vec<u8>>,
) -> Result<ProviderInputItem, ProviderError> {
    let attachment_id = image
        .attachment_id
        .map(crate::persistence::ImageAttachmentId::from_bytes)
        .ok_or(ProviderError::InvalidRequest)?;
    let bytes = data
        .get(&attachment_id)
        .filter(|bytes| bytes.len() as u64 == image.bytes)
        .ok_or(ProviderError::InvalidRequest)?;
    Ok(ProviderInputItem::MultimodalMessage {
        role: ProviderMessageRole::User,
        parts: vec![
            ProviderContentPart::Text(format!(
                "[{}] Image returned by the preceding read tool call.",
                image.display_name
            )),
            ProviderContentPart::Image {
                media_type: image.media_type,
                width: image.width,
                height: image.height,
                bytes: bytes.clone(),
            },
        ],
        phase: None,
    })
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

#[cfg(test)]
fn normalize_provider_turn(
    outcome: ProviderOutcome,
    tool_catalog_version: u16,
) -> Result<NormalizedTurn, RunFailureKind> {
    normalize_provider_turn_diagnosed(
        outcome,
        tool_catalog_version,
        &mut DebugNormalizationStage::Other,
    )
}

fn normalize_provider_turn_diagnosed(
    outcome: ProviderOutcome,
    tool_catalog_version: u16,
    stage: &mut DebugNormalizationStage,
) -> Result<NormalizedTurn, RunFailureKind> {
    normalize_tool_provider_turn(outcome, stage, |calls, stage| {
        if tool_catalog_version != TOOL_CATALOG_VERSION {
            *stage = DebugNormalizationStage::Catalog;
            return Err(ToolCallValidationError::InvalidProviderOutput);
        }
        parse_provider_calls_diagnosed(calls, tool_catalog_version, stage)
    })
}

fn normalize_subagent_provider_turn(
    outcome: ProviderOutcome,
    stage: &mut DebugNormalizationStage,
) -> Result<NormalizedTurn, RunFailureKind> {
    normalize_tool_provider_turn(outcome, stage, parse_subagent_provider_calls_diagnosed)
}

fn normalize_tool_provider_turn(
    outcome: ProviderOutcome,
    stage: &mut DebugNormalizationStage,
    parse_calls: impl FnOnce(
        Vec<ProviderToolCall>,
        &mut DebugNormalizationStage,
    ) -> Result<Vec<ValidatedProviderCall>, ToolCallValidationError>,
) -> Result<NormalizedTurn, RunFailureKind> {
    let has_tool_calls = outcome
        .output
        .iter()
        .any(|item| matches!(item, ProviderOutputItem::ToolCall(_)));
    if !has_tool_calls {
        return completed_assistant_diagnosed(outcome, stage).map(NormalizedTurn::Final);
    }
    let mut commentary = None;
    let mut calls = Vec::<ProviderToolCall>::new();
    let mut reasoning = Vec::new();
    let mut saw_call = false;
    for item in outcome.output {
        match item {
            ProviderOutputItem::AssistantMessage(message)
                if matches!(message.phase, None | Some(ProviderMessagePhase::Commentary))
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
                *stage = DebugNormalizationStage::ToolTurnMessage;
                return Err(RunFailureKind::InvalidProviderOutput);
            }
        }
    }
    let calls = parse_calls(calls, stage).map_err(map_tool_validation)?;
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

const fn map_tool_validation(error: ToolCallValidationError) -> RunFailureKind {
    match error {
        ToolCallValidationError::InvalidProviderOutput => RunFailureKind::InvalidProviderOutput,
        ToolCallValidationError::ResourceLimit => RunFailureKind::ResourceLimit,
    }
}

pub(crate) fn completed_assistant(
    outcome: ProviderOutcome,
) -> Result<CompletedAssistant, RunFailureKind> {
    completed_assistant_diagnosed(outcome, &mut DebugNormalizationStage::Other)
}

fn completed_assistant_diagnosed(
    outcome: ProviderOutcome,
    stage: &mut DebugNormalizationStage,
) -> Result<CompletedAssistant, RunFailureKind> {
    let mut final_message = None;
    for item in outcome.output {
        match item {
            ProviderOutputItem::AssistantMessage(message)
                if message.phase != Some(ProviderMessagePhase::Commentary) =>
            {
                if final_message.replace(message).is_some() {
                    *stage = DebugNormalizationStage::FinalMessageMultiple;
                    return Err(RunFailureKind::InvalidProviderOutput);
                }
            }
            ProviderOutputItem::AssistantMessage(_) | ProviderOutputItem::Reasoning(_) => {}
            ProviderOutputItem::ToolCall(_) => {
                *stage = DebugNormalizationStage::ToolInFinal;
                return Err(RunFailureKind::InvalidProviderOutput);
            }
        }
    }
    let message = final_message.ok_or_else(|| {
        *stage = DebugNormalizationStage::FinalMessageMissing;
        RunFailureKind::InvalidProviderOutput
    })?;
    if message.text.is_empty() {
        *stage = DebugNormalizationStage::FinalMessageEmpty;
        return Err(RunFailureKind::InvalidProviderOutput);
    }
    if message.text.len() > MAX_TRANSCRIPT_TEXT_BYTES {
        *stage = DebugNormalizationStage::OutputBytes;
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

const fn map_provider_failure(error: ProviderError) -> RunFailureKind {
    match error {
        ProviderError::CredentialGenerationChanged => RunFailureKind::CredentialChanged,
        ProviderError::CredentialNotConfigured => RunFailureKind::CredentialNotConfigured,
        ProviderError::CredentialReauthenticationRequired => {
            RunFailureKind::CredentialReauthenticationRequired
        }
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
        ProviderError::DataUseRestricted => RunFailureKind::DataUseRestricted,
        ProviderError::InvalidRequest | ProviderError::UnsupportedModel => RunFailureKind::Internal,
        ProviderError::MalformedCatalog
        | ProviderError::Cancelled
        | ProviderError::CredentialStoreUnavailable => RunFailureKind::Internal,
    }
}

const fn provider_failure_state(error: ProviderError) -> ProviderOperationFailureState {
    match error {
        ProviderError::DataUseRestricted
        | ProviderError::CredentialStoreUnavailable
        | ProviderError::CredentialReauthenticationRequired
        | ProviderError::AuthenticationOrEntitlement
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
pub(crate) mod tests;
