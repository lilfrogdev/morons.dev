use std::{
    collections::HashMap,
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
    persistence::{
        CompletedAssistant, DispatchOutcome, MAX_TRANSCRIPT_TEXT_BYTES, PersistenceError,
        PrepareOperationOutcome, ProviderOperationFailureState, ProviderUsage, RunFailureKind,
        RunId, RunOpenCodeService, SessionStore, TranscriptEntry,
    },
    provider::{
        OpenCodeProvider, OpenCodeResponseRequest, OpenCodeService, ProviderCancellation,
        ProviderCancellationHandle, ProviderError, ProviderInputItem, ProviderMessagePhase,
        ProviderMessageRole, ProviderOutcome, ProviderOutputItem, provider_cancellation,
    },
};

const MAX_CONCURRENT_RUNS: usize = 4;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct RunSupervisor {
    sessions: Arc<SessionStore>,
    provider: Arc<OpenCodeProvider>,
    permits: Arc<Semaphore>,
    stopping: AtomicBool,
    state: Mutex<SupervisorState>,
}

struct SupervisorState {
    controls: HashMap<RunId, ProviderCancellationHandle>,
    tasks: JoinSet<()>,
}

impl RunSupervisor {
    pub(crate) fn new(sessions: Arc<SessionStore>, provider: Arc<OpenCodeProvider>) -> Arc<Self> {
        Arc::new(Self {
            sessions,
            provider,
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_RUNS)),
            stopping: AtomicBool::new(false),
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
        let context = self.sessions.load_run_context(run_id).await?;
        let request = match build_provider_request(&context) {
            Ok(request) => request,
            Err(error) => {
                let failure = map_provider_failure(error);
                self.sessions
                    .finish_run_failure(
                        run_id,
                        None,
                        failure,
                        ProviderOperationFailureState::Failed,
                    )
                    .await?;
                return Ok(());
            }
        };
        let operation_id = match self.sessions.prepare_provider_operation(run_id).await? {
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

        match dispatch.execute(&mut cancellation, |_| {}).await {
            Ok(outcome) => match completed_assistant(outcome) {
                Ok(assistant) => {
                    self.sessions
                        .complete_run_success(run_id, operation_id, assistant)
                        .await?;
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
                }
            },
            Err(ProviderError::Cancelled) => {
                self.sessions
                    .finish_run_stopped(run_id, Some(operation_id))
                    .await?;
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
            }
        }
        Ok(())
    }

    async fn remove_control(&self, run_id: RunId) {
        self.state.lock().await.controls.remove(&run_id);
    }
}

fn build_provider_request(
    context: &crate::persistence::RunContext,
) -> Result<OpenCodeResponseRequest, ProviderError> {
    let input = context
        .entries
        .iter()
        .map(|entry| match entry {
            TranscriptEntry::UserMessage { text, .. } => ProviderInputItem::Message {
                role: ProviderMessageRole::User,
                text: text.clone(),
                phase: None,
            },
            TranscriptEntry::AssistantMessage { text, .. } => ProviderInputItem::Message {
                role: ProviderMessageRole::Assistant,
                text: text.clone(),
                phase: None,
            },
        })
        .collect();
    OpenCodeResponseRequest::new(
        to_provider_service(context.run.service),
        &context.run.model_id,
        context.run.estimated_input_tokens,
        context.run.maximum_output_tokens,
        input,
        Vec::new(),
    )
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
