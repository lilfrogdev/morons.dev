use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use tokio::{
    sync::{Mutex, OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
};

use crate::{
    persistence::{AcceptedLocalCommand, LocalCommandId, PersistenceError, SessionStore},
    provider::{ProviderCancellationHandle, provider_cancellation},
    tools::{BashToolExecutor, ToolErrorKind, ToolInput, ToolResult},
};

const MAX_CONCURRENT_COMMANDS: usize = 4;

pub(crate) struct CommandSupervisor {
    sessions: Arc<SessionStore>,
    permits: Arc<Semaphore>,
    stopping: AtomicBool,
    state: Mutex<SupervisorState>,
}

struct SupervisorState {
    controls: HashMap<LocalCommandId, ProviderCancellationHandle>,
    tasks: JoinSet<()>,
}

impl CommandSupervisor {
    pub(crate) fn new(sessions: Arc<SessionStore>) -> Arc<Self> {
        Arc::new(Self {
            sessions,
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_COMMANDS)),
            stopping: AtomicBool::new(false),
            state: Mutex::new(SupervisorState {
                controls: HashMap::new(),
                tasks: JoinSet::new(),
            }),
        })
    }

    pub(crate) fn try_reserve(&self) -> Option<OwnedSemaphorePermit> {
        if self.stopping.load(Ordering::Acquire) {
            return None;
        }
        Arc::clone(&self.permits).try_acquire_owned().ok()
    }

    pub(crate) async fn start(
        self: &Arc<Self>,
        command: AcceptedLocalCommand,
        working_directory: PathBuf,
        permit: OwnedSemaphorePermit,
    ) -> Result<(), PersistenceError> {
        let (handle, cancellation) = provider_cancellation();
        let command_id = command.id;
        let mut state = self.state.lock().await;
        while let Some(result) = state.tasks.try_join_next() {
            if let Err(error) = result {
                eprintln!("local command task failed to join: {error}");
            }
        }
        if state.controls.contains_key(&command_id) {
            return Err(PersistenceError::InvalidState {
                reason: "an accepted local command already has a supervisor task",
            });
        }
        state.controls.insert(command_id, handle);
        let supervisor = Arc::clone(self);
        state.tasks.spawn(async move {
            let _permit = permit;
            if let Err(error) = supervisor
                .execute(command, working_directory, cancellation)
                .await
            {
                eprintln!("local command execution failed: {error}");
            }
            supervisor.state.lock().await.controls.remove(&command_id);
        });
        Ok(())
    }

    async fn execute(
        &self,
        command: AcceptedLocalCommand,
        working_directory: PathBuf,
        cancellation: crate::provider::ProviderCancellation,
    ) -> Result<(), PersistenceError> {
        if !self.sessions.activate_local_command(command.id).await? {
            self.sessions
                .complete_local_command(command.id, ToolResult::error(ToolErrorKind::Cancelled))
                .await?;
            return Ok(());
        }
        let input = ToolInput::Bash {
            command: command.command,
        };
        let cancellation_for_task = cancellation.clone();
        let result = tokio::task::spawn_blocking(move || {
            BashToolExecutor::new(working_directory)
                .execute(&input, &|| cancellation_for_task.is_cancelled())
        })
        .await
        .unwrap_or_else(|_| ToolResult::error(ToolErrorKind::Uncertain));
        self.sessions
            .complete_local_command(command.id, result)
            .await?;
        Ok(())
    }

    pub(crate) async fn signal_cancellation(&self, command_id: LocalCommandId) {
        if let Some(control) = self.state.lock().await.controls.get(&command_id) {
            control.cancel();
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
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result {
                eprintln!("local command task failed during shutdown: {error}");
            }
        }
        self.state.lock().await.controls.clear();
    }
}
