use sha2::{Digest as _, Sha256};
#[cfg(test)]
use tokio::sync::oneshot;

#[cfg(test)]
use super::{MutationRequestId, SessionStore, WorkerRequest};
use super::{PersistenceError, RunId, SessionId, types::validate_user_text};

#[derive(Clone, Debug)]
pub(crate) enum SteeringChange {
    Enqueue {
        run_id: RunId,
        text: String,
    },
    Edit {
        item_id: [u8; 16],
        revision: u64,
        text: String,
    },
    Remove {
        item_id: [u8; 16],
        revision: u64,
    },
    Pause,
    Resume {
        run_id: RunId,
    },
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct SteeringMutation {
    pub request_id: MutationRequestId,
    pub session_id: SessionId,
    pub expected_revision: u64,
    pub change: SteeringChange,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SteeringReceipt {
    pub sequence: u64,
    pub queue_revision: u64,
    pub item_id: Option<[u8; 16]>,
    pub item_revision: Option<u64>,
}

#[cfg(test)]
pub(super) enum Request {
    Mutate {
        mutation: SteeringMutation,
        response: oneshot::Sender<Result<SteeringReceipt, PersistenceError>>,
    },
    Snapshot {
        session_id: SessionId,
        response: oneshot::Sender<Result<SteeringSnapshot, PersistenceError>>,
    },
    Replay {
        cursor: SteeringCursor,
        limit: u16,
        response: oneshot::Sender<Result<SteeringPage, PersistenceError>>,
    },
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SteeringCursor {
    pub session_id: SessionId,
    pub sequence: u64,
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SteeringItem {
    pub id: [u8; 16],
    pub revision: u64,
    pub enqueue_sequence: u64,
    pub text: String,
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SteeringSnapshot {
    pub cursor: SteeringCursor,
    pub revision: u64,
    pub target_run_id: Option<RunId>,
    pub paused: bool,
    pub items: Vec<SteeringItem>,
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SteeringNotice {
    pub cursor: SteeringCursor,
    pub revision: u64,
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct SteeringPage {
    pub notices: Vec<SteeringNotice>,
    pub high_water: SteeringCursor,
}

#[cfg(test)]
impl SessionStore {
    pub(crate) async fn steering_snapshot(
        &self,
        session_id: SessionId,
    ) -> Result<SteeringSnapshot, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::Steering(Request::Snapshot {
                session_id,
                response,
            }))
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub(crate) async fn steering_replay(
        &self,
        cursor: SteeringCursor,
        limit: u16,
    ) -> Result<SteeringPage, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::Steering(Request::Replay {
                cursor,
                limit,
                response,
            }))
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub(crate) async fn mutate_steering(
        &self,
        mutation: SteeringMutation,
    ) -> Result<SteeringReceipt, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::Steering(Request::Mutate {
                mutation,
                response,
            }))
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }
}

#[cfg(test)]
impl Request {
    pub(super) fn execute(self, backend: &mut super::backend::Backend) {
        match self {
            Self::Mutate { mutation, response } => {
                let _ = response.send(backend.mutate_steering(mutation));
            }
            Self::Snapshot {
                session_id,
                response,
            } => {
                let _ = response.send(backend.steering_snapshot(session_id));
            }
            Self::Replay {
                cursor,
                limit,
                response,
            } => {
                let _ = response.send(backend.steering_replay(cursor, limit));
            }
        }
    }
}

pub(super) fn validate_text(text: &str) -> Result<(), PersistenceError> {
    validate_user_text(text)?;
    if text.len() > 65536 || text.trim_start().starts_with(['!', '/']) {
        return Err(PersistenceError::InvalidInput {
            reason: "steering requires bounded message text, not a command",
        });
    }
    Ok(())
}

pub(super) fn fingerprint(
    session_id: SessionId,
    expected_revision: u64,
    change: &SteeringChange,
) -> [u8; 32] {
    use SteeringChange::{Edit, Enqueue, Pause, Remove, Resume};
    let mut digest = Sha256::new();
    digest.update(b"morons-steering-v1");
    digest.update(session_id.as_bytes());
    digest.update(expected_revision.to_be_bytes());
    match change {
        Enqueue { run_id, text } => {
            digest.update([1]);
            digest.update(run_id.as_bytes());
            digest.update(text.as_bytes());
        }
        Edit {
            item_id,
            revision,
            text,
        } => {
            digest.update([2]);
            digest.update(item_id);
            digest.update(revision.to_be_bytes());
            digest.update(text.as_bytes());
        }
        Remove { item_id, revision } => {
            digest.update([3]);
            digest.update(item_id);
            digest.update(revision.to_be_bytes());
        }
        Pause => digest.update([4]),
        Resume { run_id } => {
            digest.update([5]);
            digest.update(run_id.as_bytes());
        }
    }
    digest.finalize().into()
}
