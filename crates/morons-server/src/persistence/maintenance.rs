use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use super::{
    Backend, CompactionPlan, PersistenceError, Run, RunId, SessionId, SessionStore, WorkerRequest,
};

pub(super) fn enabled_from_environment(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_none_or(|value| value == "1")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub(crate) enum MaintenanceState {
    Prepared = 1,
    Dispatched = 2,
    Ready = 3,
    Failed = 4,
    Cancelled = 5,
    Uncertain = 6,
    Discarded = 7,
    Installed = 8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MaintenanceObservation {
    pub enabled: bool,
    pub latest: Option<MaintenanceJobObservation>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MaintenanceJobObservation {
    pub state: MaintenanceState,
    pub service: super::RunService,
    pub model_id: String,
    pub source_entry_high_water: u64,
    pub usage: Option<super::run_types::RecentProviderUsage>,
}

pub(crate) struct MaintenanceWork {
    pub id: [u8; 16],
    pub run: Run,
    pub plan: CompactionPlan,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MaintenanceResult {
    pub summary: String,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_write_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub total_tokens: u64,
}

impl SessionStore {
    pub(crate) async fn disable_maintenance(&self) -> Result<(), PersistenceError> {
        self.maintenance_request(|response| MaintenanceRequest::Disable { response })
            .await
    }
    pub(crate) fn background_compaction_enabled(&self) -> bool {
        self.maintenance_enabled
    }

    pub(crate) async fn prepare_maintenance(
        &self,
        run: RunId,
    ) -> Result<Option<MaintenanceWork>, PersistenceError> {
        self.maintenance_request(|response| MaintenanceRequest::Prepare { run, response })
            .await
    }
    pub(crate) async fn dispatch_maintenance(
        &self,
        id: [u8; 16],
    ) -> Result<bool, PersistenceError> {
        self.maintenance_request(|response| MaintenanceRequest::Dispatch { id, response })
            .await
    }
    pub(crate) async fn complete_maintenance(
        &self,
        id: [u8; 16],
        result: MaintenanceResult,
    ) -> Result<(), PersistenceError> {
        self.maintenance_request(|response| MaintenanceRequest::Complete {
            id,
            result,
            response,
        })
        .await
    }
    pub(crate) async fn finish_maintenance(
        &self,
        session: SessionId,
        failed: bool,
        discard_ready: bool,
    ) -> Result<(), PersistenceError> {
        self.maintenance_request(|response| MaintenanceRequest::Finish {
            session,
            failed,
            discard_ready,
            response,
        })
        .await
    }
    pub(crate) async fn maintenance_boundary(
        &self,
        run: RunId,
    ) -> Result<Option<SessionId>, PersistenceError> {
        self.maintenance_request(|response| MaintenanceRequest::Boundary { run, response })
            .await
    }
    async fn maintenance_request<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T, PersistenceError>>) -> MaintenanceRequest,
    ) -> Result<T, PersistenceError> {
        let (sender, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::Maintenance(build(sender)))
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }
}

pub(super) enum MaintenanceRequest {
    Disable {
        response: oneshot::Sender<Result<(), PersistenceError>>,
    },
    Prepare {
        run: RunId,
        response: oneshot::Sender<Result<Option<MaintenanceWork>, PersistenceError>>,
    },
    Dispatch {
        id: [u8; 16],
        response: oneshot::Sender<Result<bool, PersistenceError>>,
    },
    Complete {
        id: [u8; 16],
        result: MaintenanceResult,
        response: oneshot::Sender<Result<(), PersistenceError>>,
    },
    Finish {
        session: SessionId,
        failed: bool,
        discard_ready: bool,
        response: oneshot::Sender<Result<(), PersistenceError>>,
    },
    Boundary {
        run: RunId,
        response: oneshot::Sender<Result<Option<SessionId>, PersistenceError>>,
    },
}

impl MaintenanceRequest {
    pub(super) fn execute(self, backend: &mut Backend) {
        match self {
            Self::Disable { response } => {
                let _ = response.send(backend.configure_maintenance(false));
            }
            Self::Prepare { run, response } => {
                let _ = response.send(backend.prepare_maintenance(run));
            }
            Self::Dispatch { id, response } => {
                let _ = response.send(backend.dispatch_maintenance(id));
            }
            Self::Complete {
                id,
                result,
                response,
            } => {
                let _ = response.send(backend.complete_maintenance(id, result));
            }
            Self::Finish {
                session,
                failed,
                discard_ready,
                response,
            } => {
                let _ = response.send(backend.finish_maintenance(session, failed, discard_ready));
            }
            Self::Boundary { run, response } => {
                let _ = response.send(backend.maintenance_boundary(run));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::enabled_from_environment;
    use std::ffi::OsStr;

    #[test]
    fn maintenance_defaults_on_and_explicit_or_invalid_opt_out_disables_it() {
        assert!(enabled_from_environment(None));
        assert!(enabled_from_environment(Some(OsStr::new("1"))));
        for value in ["0", "", "true", "false", " 1", "01", "invalid"] {
            assert!(!enabled_from_environment(Some(OsStr::new(value))));
        }
    }
}
