use super::{PersistenceError, SessionStore, ToolCallId, WorkerRequest};
use tokio::sync::oneshot;

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub(crate) enum ChildEntryKind {
    Start = 1,
    ProviderDispatch = 2,
    ProviderResult = 3,
    ToolDispatch = 4,
    ToolResult = 5,
    Batch = 6,
    Checkpoint = 7,
    Terminal = 8,
    Interrupted = 9,
}

pub(super) struct Request {
    pub call_id: ToolCallId,
    pub child: u16,
    pub ordinal: u64,
    pub kind: ChildEntryKind,
    pub payload: Vec<u8>,
    pub previous: [u8; 32],
    pub response: oneshot::Sender<Result<[u8; 32], PersistenceError>>,
}

impl SessionStore {
    pub(crate) async fn append_child_entry(
        &self,
        call_id: ToolCallId,
        child: u16,
        ordinal: u64,
        kind: ChildEntryKind,
        payload: Vec<u8>,
        previous: [u8; 32],
    ) -> Result<[u8; 32], PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::ChildJournal(Request {
                call_id,
                child,
                ordinal,
                kind,
                payload,
                previous,
                response,
            }))
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }
}

impl Request {
    pub(super) fn execute(self, backend: &mut super::backend::Backend) {
        let result = backend.append_child_entry(
            self.call_id,
            self.child,
            self.ordinal,
            self.kind,
            &self.payload,
            self.previous,
        );
        let _ = self.response.send(result);
    }
}
