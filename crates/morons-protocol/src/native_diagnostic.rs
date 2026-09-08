use serde::{Deserialize, Serialize};
#[cfg(test)]
mod tests;

/// Ephemeral local validation guard; contains no provider-supplied detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeResponseFailure {
    Headers,
    HeaderFraming,
    ContentType,
    Redirect,
    BodyBounds,
    RoutingState,
    BodyFraming,
    SseFraming,
    Json,
    EventBounds,
    EventEnvelope,
    EventKind,
    SequenceField,
    Sequence,
    Lifecycle,
    ResponseIdentity,
    ResponseModel,
    TextDelta,
    RefusalDelta,
    ArgumentDelta,
    CompletedEnvelope,
    CompletedIdentity,
    Usage,
    AssistantMessage,
    Reasoning,
    FunctionCall,
    OutputConsistency,
    Termination,
}
