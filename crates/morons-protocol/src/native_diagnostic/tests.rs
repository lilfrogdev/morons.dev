use super::*;
use crate::{ApplicationEvent, RunId, SessionId};
use serde_json::json;

#[test]
fn native_diagnostic_wire_is_closed_ephemeral_and_has_no_detail_channel() {
    let reasons = [
        NativeResponseFailure::Headers,
        NativeResponseFailure::HeaderFraming,
        NativeResponseFailure::ContentType,
        NativeResponseFailure::Redirect,
        NativeResponseFailure::BodyBounds,
        NativeResponseFailure::RoutingState,
        NativeResponseFailure::BodyFraming,
        NativeResponseFailure::SseFraming,
        NativeResponseFailure::Json,
        NativeResponseFailure::EventBounds,
        NativeResponseFailure::EventEnvelope,
        NativeResponseFailure::EventKind,
        NativeResponseFailure::SequenceField,
        NativeResponseFailure::Sequence,
        NativeResponseFailure::Lifecycle,
        NativeResponseFailure::ResponseIdentity,
        NativeResponseFailure::ResponseModel,
        NativeResponseFailure::TextDelta,
        NativeResponseFailure::RefusalDelta,
        NativeResponseFailure::ArgumentDelta,
        NativeResponseFailure::CompletedEnvelope,
        NativeResponseFailure::CompletedIdentity,
        NativeResponseFailure::Usage,
        NativeResponseFailure::AssistantMessage,
        NativeResponseFailure::Reasoning,
        NativeResponseFailure::FunctionCall,
        NativeResponseFailure::OutputConsistency,
        NativeResponseFailure::Termination,
    ];
    for reason in reasons {
        let event = ApplicationEvent::SessionNativeResponseDiagnostic {
            session_id: SessionId::from_bytes([1; 16]),
            run_id: RunId::from_bytes([2; 16]),
            reason,
        };
        let encoded = serde_json::to_value(&event).unwrap();
        assert_eq!(
            serde_json::from_value::<ApplicationEvent>(encoded.clone()).unwrap(),
            event
        );
        assert!(event.session_cursor().is_none());
        assert!(event.session_catalog_cursor().is_none());
        for bad in [
            json!("PRIVATE-unknown"),
            json!({"usage":"PRIVATE-detail"}),
            json!(null),
            json!(["usage"]),
        ] {
            let mut hostile = encoded.clone();
            hostile["reason"] = bad;
            assert!(serde_json::from_value::<ApplicationEvent>(hostile).is_err());
        }
        for key in ["detail", "headers", "tokens", "cursor", "provider_body"] {
            let mut hostile = encoded.clone();
            hostile[key] = json!("PRIVATE");
            assert!(serde_json::from_value::<ApplicationEvent>(hostile).is_err());
        }
        let mut absent = encoded;
        absent.as_object_mut().unwrap().remove("reason");
        assert!(serde_json::from_value::<ApplicationEvent>(absent).is_err());
    }
    assert_eq!(
        serde_json::to_value(NativeResponseFailure::ResponseModel).unwrap(),
        json!("response_model")
    );
}
