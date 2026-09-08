use crate::{
    persistence::{
        PersistenceError, RunId, SessionId, SessionStore, maintenance::MaintenanceResult,
    },
    provider::{
        ProviderCancellation, ProviderCancellationHandle, dispatch::ModelProviders,
        provider_cancellation,
    },
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex, Semaphore, watch},
    task::JoinHandle,
    time,
};

pub(crate) struct MaintenanceSupervisor {
    sessions: Arc<SessionStore>,
    provider: Arc<ModelProviders>,
    slot: Arc<Semaphore>,
    task: Mutex<Option<Active>>,
    stopping: AtomicBool,
    deadline: Duration,
    shutdown: watch::Sender<bool>,
}
#[cfg(test)]
mod tests;

struct Active {
    session: SessionId,
    cancel: ProviderCancellationHandle,
    task: JoinHandle<()>,
}

impl MaintenanceSupervisor {
    pub(crate) fn new(
        sessions: Arc<SessionStore>,
        provider: Arc<ModelProviders>,
        shutdown: watch::Sender<bool>,
    ) -> Arc<Self> {
        Arc::new(Self {
            sessions,
            provider,
            shutdown,
            slot: Arc::new(Semaphore::new(1)),
            task: Mutex::new(None),
            stopping: AtomicBool::new(false),
            deadline: Duration::from_secs(180),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_deadline_for_test(
        sessions: Arc<SessionStore>,
        provider: Arc<ModelProviders>,
        shutdown: watch::Sender<bool>,
        deadline: Duration,
    ) -> Arc<Self> {
        let mut supervisor = Self::new(sessions, provider, shutdown);
        Arc::get_mut(&mut supervisor).unwrap().deadline = deadline;
        supervisor
    }

    pub(crate) async fn maybe_start(self: &Arc<Self>, session: SessionId, run: RunId) {
        if !self.sessions.background_compaction_enabled()
            || self.stopping.load(Ordering::Acquire)
            || *self.shutdown.borrow()
        {
            return;
        }
        let Ok(permit) = Arc::clone(&self.slot).try_acquire_owned() else {
            return;
        };
        let Ok(mut state) = self.task.try_lock() else {
            return;
        };
        if self.stopping.load(Ordering::Acquire) || *self.shutdown.borrow() {
            return;
        }
        if let Some(previous) = state.take()
            && previous.task.await.is_err()
        {
            self.fatal();
            return;
        }
        let (cancel, mut cancellation) = provider_cancellation();
        let supervisor = Arc::clone(self);
        let task = tokio::spawn(async move {
            let _permit = permit;
            let operation_cancel = cancellation.clone();
            let outcome = tokio::select! {
                biased;
                _ = cancellation.cancelled() => Ok(()),
                result = time::timeout(supervisor.deadline, supervisor.execute(run, session, operation_cancel)) => result.unwrap_or(Ok(())),
            };
            if outcome.is_err() {
                supervisor.fatal();
            }
            if supervisor
                .sessions
                .finish_maintenance(session, false, false)
                .await
                .is_err()
            {
                supervisor.fatal();
            }
        });
        *state = Some(Active {
            session,
            cancel,
            task,
        });
    }

    async fn execute(
        &self,
        run: RunId,
        session: SessionId,
        mut cancellation: ProviderCancellation,
    ) -> Result<(), PersistenceError> {
        let Some(work) = self.sessions.prepare_maintenance(run).await? else {
            return Ok(());
        };
        if work.run.session_id != session {
            return Err(PersistenceError::InvalidState {
                reason: "maintenance supervisor scope does not match the triggering run",
            });
        }
        let built = (|| {
            let turn = self.provider.turn(
                work.run.service.model_service(),
                &work.run.model_id,
                work.id,
                *work.run.id.as_bytes(),
                work.run.credential_generation,
            )?;
            let request = turn.request(crate::run_supervisor::build_compaction_input(
                &work.run, &work.plan,
            )?)?;
            Ok::<_, crate::provider::ProviderError>((turn, request))
        })();
        let (mut turn, request) = match built {
            Ok(prepared) => prepared,
            Err(_) => return self.sessions.finish_maintenance(session, true, false).await,
        };
        let policy = self.sessions.data_use_policy().await?.restrictions;
        let dispatch = match self
            .provider
            .prepare_dispatch(&mut turn, &request, policy, &mut cancellation)
            .await
        {
            Ok(dispatch) => dispatch,
            Err(crate::provider::ProviderError::Cancelled) => return Ok(()),
            Err(crate::provider::ProviderError::CredentialStoreUnavailable) => {
                return Err(PersistenceError::InvalidState {
                    reason: "provider credential storage is unavailable",
                });
            }
            Err(_) => return self.sessions.finish_maintenance(session, true, false).await,
        };
        if cancellation.is_cancelled() || !self.sessions.dispatch_maintenance(work.id).await? {
            return Ok(());
        }
        let Ok(outcome) = dispatch.execute(policy, &mut cancellation, |_| {}).await else {
            return Ok(());
        };
        let usage = outcome.usage;
        let Ok(assistant) = crate::run_supervisor::completed_assistant(outcome) else {
            return Ok(());
        };
        if assistant.refusal {
            return Ok(());
        }
        self.sessions
            .complete_maintenance(
                work.id,
                MaintenanceResult {
                    summary: assistant.text,
                    input_tokens: usage.input_tokens,
                    cached_input_tokens: usage.cached_input_tokens,
                    cache_write_input_tokens: usage.cache_write_input_tokens,
                    output_tokens: usage.output_tokens,
                    reasoning_output_tokens: usage.reasoning_output_tokens,
                    total_tokens: usage.total_tokens,
                },
            )
            .await
    }

    pub(crate) async fn cancel_session(&self, session: SessionId) -> Result<(), PersistenceError> {
        let mut state = self.task.lock().await;
        if let Some(active) = state.as_mut().filter(|active| active.session == session) {
            self.drain(active).await;
            state.take();
        }
        let result = self.sessions.finish_maintenance(session, false, true).await;
        if result.is_err() {
            self.fatal();
        }
        result
    }

    pub(crate) async fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        if self.sessions.disable_maintenance().await.is_err() {
            self.fatal();
        }
        let mut state = self.task.lock().await;
        if let Some(active) = state.as_mut() {
            self.drain(active).await;
            let session = active.session;
            state.take();
            if self
                .sessions
                .finish_maintenance(session, false, false)
                .await
                .is_err()
            {
                self.fatal();
            }
        }
        if self.sessions.disable_maintenance().await.is_err() {
            self.fatal();
        }
    }

    async fn drain(&self, active: &mut Active) {
        active.cancel.cancel();
        match time::timeout(Duration::from_secs(10), &mut active.task).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => self.fatal(),
            Err(_) => {
                active.task.abort();
                let _ = (&mut active.task).await;
            }
        }
    }

    fn fatal(&self) {
        eprintln!(
            "background compaction persistence or supervision failed; requesting server shutdown"
        );
        self.stopping.store(true, Ordering::Release);
        self.shutdown.send_replace(true);
    }
}
