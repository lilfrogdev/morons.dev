use super::*;
use morons_protocol::{
    MessageId, ModelService, NativeResponseFailure, RunFailureKind, RunState, RunSummary,
};

fn cursor(session: SessionId, sequence: u64) -> SessionEventCursor {
    let mut bytes = [0; 24];
    bytes[..16].copy_from_slice(session.as_bytes());
    bytes[16..].copy_from_slice(&sequence.to_be_bytes());
    SessionEventCursor::from_bytes(bytes)
}
fn fixture() -> (SessionSubscription<tokio::io::DuplexStream>, RunSummary) {
    let session_id = SessionId::from_bytes([1; 16]);
    let run = RunSummary {
        id: RunId::from_bytes([2; 16]),
        session_id,
        user_message_id: MessageId::from_bytes([3; 16]),
        service: ModelService::OpenAiChatGpt,
        model_id: "gpt-5.5".into(),
        protocol_revision: 5,
        credential_generation: 1,
        context_policy_version: 4,
        tool_catalog_version: 10,
        tool_limits_version: 1,
        state: RunState::Failed,
        cancellation_requested: false,
        failure: Some(RunFailureKind::ProviderProtocol),
        accepted_at_milliseconds: 1,
        updated_at_milliseconds: 2,
    };
    let subscription = SessionSubscription {
        connection: tokio::io::duplex(1024).0,
        session_id,
        cursor: cursor(session_id, 0),
        active_delta_run: None,
        terminal_delta_run: None,
        native_failure_run: None,
        delta_sequence: 0,
        usable: true,
    };
    (subscription, run)
}
#[test]
fn native_diagnostic_subscription_requires_exact_preceding_terminal_scope_once() {
    for case in [
        "valid", "early", "session", "run", "success", "opencode", "newer",
    ] {
        let (mut subscription, mut run) = fixture();
        if case == "success" {
            run.state = RunState::Succeeded;
            run.failure = None;
        }
        if case == "opencode" {
            run.service = ModelService::Zen;
        }
        if case != "early" {
            subscription
                .validate_event(&ApplicationEvent::SessionRunChanged {
                    cursor: cursor(run.session_id, 1),
                    run: run.clone(),
                })
                .unwrap();
        }
        if case == "newer" {
            let mut newer = run.clone();
            newer.id = RunId::from_bytes([4; 16]);
            newer.state = RunState::Active;
            newer.failure = None;
            subscription
                .validate_event(&ApplicationEvent::SessionRunChanged {
                    cursor: cursor(run.session_id, 2),
                    run: newer,
                })
                .unwrap();
        }
        let event = ApplicationEvent::SessionNativeResponseDiagnostic {
            session_id: if case == "session" {
                SessionId::from_bytes([9; 16])
            } else {
                run.session_id
            },
            run_id: if case == "run" {
                RunId::from_bytes([9; 16])
            } else {
                run.id
            },
            reason: NativeResponseFailure::Usage,
        };
        let before = subscription.cursor();
        let result = subscription.validate_event(&event);
        assert_eq!(subscription.cursor(), before);
        if case == "valid" {
            result.unwrap();
            // A repeated terminal update cannot re-arm a consumed diagnostic.
            subscription
                .validate_event(&ApplicationEvent::SessionRunChanged {
                    cursor: cursor(run.session_id, 2),
                    run,
                })
                .unwrap();
            assert!(subscription.validate_event(&event).is_err());
        } else {
            assert!(matches!(
                result,
                Err(ApplicationClientError::EventScopeMismatch)
            ));
            assert!(!subscription.usable);
        }
    }
}
