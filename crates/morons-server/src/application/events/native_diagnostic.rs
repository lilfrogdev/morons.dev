use super::*;
use crate::provider::response_diagnostic::ResponseStage;

#[derive(Clone, Copy, Debug)]
pub(crate) struct NativeResponseDiagnostic {
    pub(crate) session_id: SessionId,
    pub(crate) run_id: RunId,
    pub(crate) reason: ResponseStage,
}

impl NativeResponseDiagnostic {
    pub(crate) fn into_event(self) -> ApplicationEvent {
        use morons_protocol::NativeResponseFailure as Wire;
        let reason = match self.reason {
            ResponseStage::Headers => Wire::Headers,
            ResponseStage::HeaderFraming => Wire::HeaderFraming,
            ResponseStage::ContentType => Wire::ContentType,
            ResponseStage::Redirect => Wire::Redirect,
            ResponseStage::BodyBounds => Wire::BodyBounds,
            ResponseStage::RoutingState => Wire::RoutingState,
            ResponseStage::BodyFraming => Wire::BodyFraming,
            ResponseStage::SseFraming => Wire::SseFraming,
            ResponseStage::Json => Wire::Json,
            ResponseStage::EventBounds => Wire::EventBounds,
            ResponseStage::EventEnvelope => Wire::EventEnvelope,
            ResponseStage::EventKind => Wire::EventKind,
            ResponseStage::SequenceField => Wire::SequenceField,
            ResponseStage::Sequence => Wire::Sequence,
            ResponseStage::Lifecycle => Wire::Lifecycle,
            ResponseStage::ResponseIdentity => Wire::ResponseIdentity,
            ResponseStage::ResponseModel => Wire::ResponseModel,
            ResponseStage::TextDelta => Wire::TextDelta,
            ResponseStage::RefusalDelta => Wire::RefusalDelta,
            ResponseStage::ArgumentDelta => Wire::ArgumentDelta,
            ResponseStage::CompletedEnvelope => Wire::CompletedEnvelope,
            ResponseStage::CompletedIdentity => Wire::CompletedIdentity,
            ResponseStage::Usage => Wire::Usage,
            ResponseStage::AssistantMessage => Wire::AssistantMessage,
            ResponseStage::Reasoning => Wire::Reasoning,
            ResponseStage::FunctionCall => Wire::FunctionCall,
            ResponseStage::OutputConsistency => Wire::OutputConsistency,
            ResponseStage::Termination => Wire::Termination,
        };
        ApplicationEvent::SessionNativeResponseDiagnostic {
            session_id: morons_protocol::SessionId::from_bytes(*self.session_id.as_bytes()),
            run_id: morons_protocol::RunId::from_bytes(*self.run_id.as_bytes()),
            reason,
        }
    }
}
