use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProviderError {
    InvalidRequest,
    UnsupportedModel,
    CredentialGenerationChanged,
    CredentialNotConfigured,
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

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidRequest => "the provider request is invalid",
            Self::UnsupportedModel => "the selected provider model is unsupported",
            Self::CredentialGenerationChanged => "the provider credential generation changed",
            Self::CredentialNotConfigured => "the provider credential is not configured",
            Self::Transport => "the provider transport failed",
            Self::ResponseHeaderTimeout => "the provider response headers timed out",
            Self::StreamInactivityTimeout => "the provider response stream became inactive",
            Self::TotalTimeout => "the provider operation timed out",
            Self::Cancelled => "the provider operation was cancelled",
            Self::RedirectDenied => "the provider attempted to redirect the request",
            Self::UnexpectedContentType => "the provider returned an unexpected content type",
            Self::AuthenticationOrEntitlement => {
                "the provider rejected authentication or account entitlement"
            }
            Self::RateLimited => "the provider rate limit was reached",
            Self::Unavailable => "the provider is unavailable",
            Self::RequestRejected => "the provider rejected the request",
            Self::ProviderExecutionFailed => "the provider failed while generating a response",
            Self::MalformedCatalog => "the provider model catalog is malformed",
            Self::MalformedResponse => "the provider response is malformed",
            Self::ResponseLimitExceeded => "the provider response exceeded a resource limit",
            Self::IncompleteResponse => "the provider response was incomplete",
        })
    }
}

impl Error for ProviderError {}
