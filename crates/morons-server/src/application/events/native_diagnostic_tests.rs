use super::*;
use crate::provider::response_diagnostic::ResponseStage;

#[tokio::test]
async fn native_diagnostic_hub_is_bounded_scoped_and_does_not_replay_to_new_subscribers() {
    let hub = SessionEventHub::new();
    let mut receiver = hub.subscribe_native_diagnostics();
    let diagnostic = NativeResponseDiagnostic {
        session_id: SessionId::from_bytes([1; 16]),
        run_id: RunId::from_bytes([2; 16]),
        reason: ResponseStage::Usage,
    };
    for _ in 0..65 {
        hub.publish_native_diagnostic(diagnostic);
    }
    assert!(matches!(
        receiver.recv().await,
        Err(broadcast::error::RecvError::Lagged(1))
    ));
    let mut later = hub.subscribe_native_diagnostics();
    assert!(matches!(
        later.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    let mut subscription = SessionSubscription {
        session_id: diagnostic.session_id,
        cursor: SessionEventCursor::new(diagnostic.session_id, 0),
        notifications: watch::channel(0).1,
        assistant_deltas: hub.subscribe_assistant_deltas(),
        native_diagnostics: receiver,
        native_protocol_failure: false,
        active_run: Some(diagnostic.run_id),
        terminal_run: None,
    };
    assert!(!subscription.accepts_native_diagnostic(&diagnostic));
    subscription.active_run = None;
    subscription.terminal_run = Some(diagnostic.run_id);
    subscription.native_protocol_failure = true;
    assert!(subscription.accepts_native_diagnostic(&diagnostic));
    assert!(
        !subscription.accepts_native_diagnostic(&NativeResponseDiagnostic {
            session_id: SessionId::from_bytes([3; 16]),
            ..diagnostic
        })
    );
    assert!(
        !subscription.accepts_native_diagnostic(&NativeResponseDiagnostic {
            run_id: RunId::from_bytes([3; 16]),
            ..diagnostic
        })
    );
    subscription.active_run = Some(RunId::from_bytes([4; 16]));
    assert!(!subscription.accepts_native_diagnostic(&diagnostic));
    let event = diagnostic.into_event();
    assert!(matches!(
        event,
        ApplicationEvent::SessionNativeResponseDiagnostic {
            reason: morons_protocol::NativeResponseFailure::Usage,
            ..
        }
    ));
    assert!(event.session_cursor().is_none());
}
