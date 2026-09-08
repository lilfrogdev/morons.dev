use morons_protocol::NativeResponseFailure;

pub(super) fn native_response_message(reason: NativeResponseFailure) -> &'static str {
    macro_rules! message {
        ($label:literal) => {
            concat!(
                "Native response rejected (",
                $label,
                "). Nothing was retried; remote work may have occurred."
            )
        };
    }
    match reason {
        NativeResponseFailure::Headers => message!("headers"),
        NativeResponseFailure::HeaderFraming => message!("header-framing"),
        NativeResponseFailure::ContentType => message!("content-type"),
        NativeResponseFailure::Redirect => message!("redirect"),
        NativeResponseFailure::BodyBounds => message!("body-bounds"),
        NativeResponseFailure::RoutingState => message!("routing-state"),
        NativeResponseFailure::BodyFraming => message!("body-framing"),
        NativeResponseFailure::SseFraming => message!("sse-framing"),
        NativeResponseFailure::Json => message!("json"),
        NativeResponseFailure::EventBounds => message!("event-bounds"),
        NativeResponseFailure::EventEnvelope => message!("event-envelope"),
        NativeResponseFailure::EventKind => message!("event-kind"),
        NativeResponseFailure::SequenceField => message!("sequence-field"),
        NativeResponseFailure::Sequence => message!("sequence"),
        NativeResponseFailure::Lifecycle => message!("lifecycle"),
        NativeResponseFailure::ResponseIdentity => message!("response-identity"),
        NativeResponseFailure::ResponseModel => message!("response-model"),
        NativeResponseFailure::TextDelta => message!("text-delta"),
        NativeResponseFailure::RefusalDelta => message!("refusal-delta"),
        NativeResponseFailure::ArgumentDelta => message!("argument-delta"),
        NativeResponseFailure::CompletedEnvelope => message!("completed-envelope"),
        NativeResponseFailure::CompletedIdentity => message!("completed-identity"),
        NativeResponseFailure::Usage => message!("usage"),
        NativeResponseFailure::AssistantMessage => message!("assistant-message"),
        NativeResponseFailure::Reasoning => message!("reasoning"),
        NativeResponseFailure::FunctionCall => message!("function-call"),
        NativeResponseFailure::OutputConsistency => message!("output-consistency"),
        NativeResponseFailure::Termination => message!("termination"),
    }
}
