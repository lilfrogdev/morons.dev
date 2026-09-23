use std::{fmt, sync::Arc};
mod native_diagnostic;
#[cfg(test)]
mod native_diagnostic_tests;
pub(crate) use native_diagnostic::NativeResponseDiagnostic;

use morons_protocol::{
    ApplicationEvent, SessionCatalogEventCursor as ProtocolSessionCatalogEventCursor,
    SessionEventCursor as ProtocolSessionEventCursor,
};
use tokio::sync::{broadcast, watch};

use super::{
    DeliveredSessionEvent,
    conversions::{
        to_persistence_run_id, to_protocol_catalog_cursor, to_protocol_session_event_cursor,
    },
};
use crate::persistence::{RunId, SessionCatalogEventCursor, SessionEventCursor, SessionId};

const ASSISTANT_DELTA_QUEUE_CAPACITY: usize = 64;

pub(crate) struct SessionEventHub {
    compactions: watch::Sender<std::collections::HashMap<SessionId, RunId>>,
    assistant_deltas: broadcast::Sender<AssistantDelta>,
    native_diagnostics: broadcast::Sender<NativeResponseDiagnostic>,
}

#[derive(Clone)]
pub(crate) struct AssistantDelta {
    pub(crate) session_id: SessionId,
    pub(crate) run_id: RunId,
    pub(crate) sequence: u64,
    pub(crate) delta: String,
    pub(crate) refusal: bool,
}

impl fmt::Debug for AssistantDelta {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AssistantDelta")
            .field("session_id", &self.session_id)
            .field("run_id", &self.run_id)
            .field("sequence", &self.sequence)
            .field("delta_bytes", &self.delta.len())
            .field("refusal", &self.refusal)
            .finish()
    }
}

pub(crate) struct SessionCatalogSubscription {
    pub(crate) cursor: SessionCatalogEventCursor,
    pub(crate) notifications: watch::Receiver<u64>,
}

pub(crate) struct SessionSubscription {
    pub(crate) session_id: SessionId,
    pub(crate) cursor: SessionEventCursor,
    pub(crate) notifications: watch::Receiver<u64>,
    pub(crate) compactions: watch::Receiver<std::collections::HashMap<SessionId, RunId>>,
    pub(crate) assistant_deltas: broadcast::Receiver<AssistantDelta>,
    pub(crate) native_diagnostics: broadcast::Receiver<NativeResponseDiagnostic>,
    pub(super) native_protocol_failure: bool,
    pub(super) active_run: Option<RunId>,
    pub(super) terminal_run: Option<RunId>,
}

impl SessionCatalogSubscription {
    pub(crate) fn protocol_cursor(&self) -> ProtocolSessionCatalogEventCursor {
        to_protocol_catalog_cursor(self.cursor)
    }

    pub(crate) fn advance(&mut self, cursor: SessionCatalogEventCursor) {
        self.cursor = cursor;
    }
}

impl SessionSubscription {
    pub(crate) fn protocol_cursor(&self) -> ProtocolSessionEventCursor {
        to_protocol_session_event_cursor(self.cursor)
    }

    pub(crate) fn advance(&mut self, event: &DeliveredSessionEvent) {
        self.cursor = event.cursor;
        if let Some(ApplicationEvent::SessionRunChanged { run, .. }) = &event.event {
            let run_id = to_persistence_run_id(run.id);
            self.native_protocol_failure = run.state == morons_protocol::RunState::Failed
                && run.service == morons_protocol::ModelService::OpenAiChatGpt
                && run.failure == Some(morons_protocol::RunFailureKind::ProviderProtocol);
            if run.state.is_terminal() {
                if self.active_run == Some(run_id) {
                    self.active_run = None;
                }
                self.terminal_run = Some(run_id);
            } else {
                self.active_run = Some(run_id);
                self.terminal_run = None;
            }
        }
    }

    pub(crate) fn accepts_compaction(&self, run: RunId) -> bool {
        self.terminal_run != Some(run) && self.active_run.is_none_or(|active| active == run)
    }

    pub(crate) fn accepts_native_diagnostic(&self, diagnostic: &NativeResponseDiagnostic) -> bool {
        diagnostic.session_id == self.session_id
            && self.native_protocol_failure
            && self.active_run.is_none()
            && self.terminal_run == Some(diagnostic.run_id)
    }

    pub(crate) fn accepts_delta(&mut self, delta: &AssistantDelta) -> bool {
        if delta.session_id != self.session_id
            || self.terminal_run == Some(delta.run_id)
            || self.active_run.is_some_and(|run_id| run_id != delta.run_id)
        {
            return false;
        }
        self.active_run = Some(delta.run_id);
        true
    }
}

impl SessionEventHub {
    pub(crate) fn new() -> Arc<Self> {
        let (assistant_deltas, _) = broadcast::channel(ASSISTANT_DELTA_QUEUE_CAPACITY);
        let (native_diagnostics, _) = broadcast::channel(64);
        Arc::new(Self {
            compactions: watch::channel(std::collections::HashMap::new()).0,
            assistant_deltas,
            native_diagnostics,
        })
    }

    pub(crate) fn compaction_started(
        self: &Arc<Self>,
        session: SessionId,
        run: RunId,
    ) -> CompactionActivityGuard {
        self.compactions.send_modify(|active| {
            active.insert(session, run);
        });
        CompactionActivityGuard {
            hub: Arc::clone(self),
            session,
            run,
        }
    }

    pub(super) fn subscribe_compactions(
        &self,
    ) -> watch::Receiver<std::collections::HashMap<SessionId, RunId>> {
        self.compactions.subscribe()
    }

    pub(crate) fn publish_native_diagnostic(&self, diagnostic: NativeResponseDiagnostic) {
        let _ = self.native_diagnostics.send(diagnostic);
    }

    pub(super) fn subscribe_native_diagnostics(
        &self,
    ) -> broadcast::Receiver<NativeResponseDiagnostic> {
        self.native_diagnostics.subscribe()
    }

    pub(crate) fn publish_assistant_delta(&self, delta: AssistantDelta) {
        let _ = self.assistant_deltas.send(delta);
    }

    pub(super) fn subscribe_assistant_deltas(&self) -> broadcast::Receiver<AssistantDelta> {
        self.assistant_deltas.subscribe()
    }
}

pub(crate) struct CompactionActivityGuard {
    hub: Arc<SessionEventHub>,
    session: SessionId,
    run: RunId,
}

impl Drop for CompactionActivityGuard {
    fn drop(&mut self) {
        self.hub.compactions.send_modify(|active| {
            if active.get(&self.session) == Some(&self.run) {
                active.remove(&self.session);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_activity_reconnects_and_clears_on_scope_exit() {
        let hub = SessionEventHub::new();
        let session = SessionId::from_bytes([1; 16]);
        let run = RunId::from_bytes([2; 16]);
        let guard = hub.compaction_started(session, run);
        let receiver = hub.subscribe_compactions();
        assert_eq!(receiver.borrow().get(&session), Some(&run));
        drop(guard);
        assert!(receiver.has_changed().unwrap());
        assert!(receiver.borrow().is_empty());
    }

    #[test]
    fn hidden_historical_events_advance_the_durable_cursor() {
        let session_id = SessionId::from_bytes([0x10; 16]);
        let first = SessionEventCursor::new(session_id, 1);
        let second = SessionEventCursor::new(session_id, 2);
        let (_notifications, receiver) = watch::channel(0);
        let (_deltas, assistant_deltas) = broadcast::channel(1);
        let mut subscription = SessionSubscription {
            session_id,
            cursor: first,
            notifications: receiver,
            assistant_deltas,
            compactions: watch::channel(std::collections::HashMap::new()).1,
            native_diagnostics: broadcast::channel(1).1,
            native_protocol_failure: false,
            active_run: None,
            terminal_run: None,
        };
        subscription.advance(&DeliveredSessionEvent {
            cursor: second,
            event: None,
        });
        assert_eq!(subscription.cursor, second);
        assert_eq!(subscription.active_run, None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn assistant_delta_broadcast_is_redacted_and_multi_subscriber() {
        let hub = SessionEventHub::new();
        let mut first = hub.subscribe_assistant_deltas();
        let mut second = hub.subscribe_assistant_deltas();
        let delta = AssistantDelta {
            session_id: SessionId::from_bytes([0x11; 16]),
            run_id: RunId::from_bytes([0x22; 16]),
            sequence: 1,
            delta: "sensitive partial output".to_owned(),
            refusal: false,
        };
        let debug = format!("{delta:?}");
        assert!(!debug.contains("sensitive partial output"));
        assert!(debug.contains("delta_bytes"));
        hub.publish_assistant_delta(delta.clone());
        assert_eq!(
            first
                .recv()
                .await
                .expect("first subscriber should receive")
                .sequence,
            1
        );
        assert_eq!(
            second
                .recv()
                .await
                .expect("second subscriber should receive")
                .delta,
            delta.delta
        );
    }
}
