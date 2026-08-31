use std::{fmt, sync::Arc};

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
    assistant_deltas: broadcast::Sender<AssistantDelta>,
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
    pub(crate) assistant_deltas: broadcast::Receiver<AssistantDelta>,
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
        if let ApplicationEvent::SessionRunChanged { run, .. } = &event.event {
            let run_id = to_persistence_run_id(run.id);
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
        Arc::new(Self { assistant_deltas })
    }

    pub(crate) fn publish_assistant_delta(&self, delta: AssistantDelta) {
        let _ = self.assistant_deltas.send(delta);
    }

    pub(super) fn subscribe_assistant_deltas(&self) -> broadcast::Receiver<AssistantDelta> {
        self.assistant_deltas.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
