//! Server-owned diagnostic vocabulary, independent of provider wire objects and IPC DTOs.
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WebFailure {
    pub stage: WebStage,
    pub category: WebCategory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WebStage {
    Admission,
    Request,
    Headers,
    HttpStatus,
    ContentType,
    BodyBounds,
    BodyFraming,
    Sse,
    Json,
    EventEnvelope,
    EventKind,
    Sequence,
    Lifecycle,
    ResponseIdentity,
    ResponseModel,
    OutputItem,
    OutputConsistency,
    Usage,
    SearchAction,
    AssistantMessage,
    Citation,
    Reasoning,
    Completion,
    Termination,
}
impl WebStage {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Admission => "admission",
            Self::Request => "request",
            Self::Headers => "headers",
            Self::HttpStatus => "http-status",
            Self::ContentType => "content-type",
            Self::BodyBounds => "body-bounds",
            Self::BodyFraming => "body-framing",
            Self::Sse => "sse",
            Self::Json => "json",
            Self::EventEnvelope => "event-envelope",
            Self::EventKind => "event-kind",
            Self::Sequence => "sequence",
            Self::Lifecycle => "lifecycle",
            Self::ResponseIdentity => "response-identity",
            Self::ResponseModel => "response-model",
            Self::OutputItem => "output-item",
            Self::OutputConsistency => "output-consistency",
            Self::Usage => "usage",
            Self::SearchAction => "search-action",
            Self::AssistantMessage => "assistant-message",
            Self::Citation => "citation",
            Self::Reasoning => "reasoning",
            Self::Completion => "completion",
            Self::Termination => "termination",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WebCategory {
    InvalidRequest,
    UnsupportedModel,
    DataUseRestricted,
    CredentialGenerationChanged,
    CredentialNotConfigured,
    CredentialReauthenticationRequired,
    CredentialStoreUnavailable,
    Transport,
    ResponseHeaderTimeout,
    StreamInactivityTimeout,
    TotalTimeout,
    Cancelled,
    RedirectDenied,
    UnexpectedContentType,
    AuthenticationOrEntitlement,
    RateLimited,
    Unavailable,
    RequestRejected,
    ProviderExecutionFailed,
    MalformedCatalog,
    MalformedResponse,
    ResponseLimitExceeded,
    IncompleteResponse,
}
impl WebCategory {
    pub const fn label(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid-request",
            Self::UnsupportedModel => "unsupported-model",
            Self::DataUseRestricted => "data-use-restricted",
            Self::CredentialGenerationChanged => "credential-generation-changed",
            Self::CredentialNotConfigured => "credential-not-configured",
            Self::CredentialReauthenticationRequired => "reauthentication-required",
            Self::CredentialStoreUnavailable => "credential-store-unavailable",
            Self::Transport => "transport",
            Self::ResponseHeaderTimeout => "header-timeout",
            Self::StreamInactivityTimeout => "idle-timeout",
            Self::TotalTimeout => "total-timeout",
            Self::Cancelled => "cancelled",
            Self::RedirectDenied => "redirect-denied",
            Self::UnexpectedContentType => "unexpected-content-type",
            Self::AuthenticationOrEntitlement => "authentication-or-entitlement",
            Self::RateLimited => "rate-limited",
            Self::Unavailable => "unavailable",
            Self::RequestRejected => "request-rejected",
            Self::ProviderExecutionFailed => "provider-execution-failed",
            Self::MalformedCatalog => "malformed-catalog",
            Self::MalformedResponse => "malformed-response",
            Self::ResponseLimitExceeded => "response-limit",
            Self::IncompleteResponse => "incomplete-response",
        }
    }
}
