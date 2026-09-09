use crate::{
    provider::ProviderError,
    web_diagnostic::{WebCategory, WebFailure, WebStage},
};

pub(super) fn failure(stage: WebStage, error: ProviderError) -> WebFailure {
    let category = match error {
        ProviderError::InvalidRequest => WebCategory::InvalidRequest,
        ProviderError::UnsupportedModel => WebCategory::UnsupportedModel,
        ProviderError::DataUseRestricted => WebCategory::DataUseRestricted,
        ProviderError::CredentialGenerationChanged => WebCategory::CredentialGenerationChanged,
        ProviderError::CredentialNotConfigured => WebCategory::CredentialNotConfigured,
        ProviderError::CredentialReauthenticationRequired => {
            WebCategory::CredentialReauthenticationRequired
        }
        ProviderError::CredentialStoreUnavailable => WebCategory::CredentialStoreUnavailable,
        ProviderError::Transport => WebCategory::Transport,
        ProviderError::ResponseHeaderTimeout => WebCategory::ResponseHeaderTimeout,
        ProviderError::StreamInactivityTimeout => WebCategory::StreamInactivityTimeout,
        ProviderError::TotalTimeout => WebCategory::TotalTimeout,
        ProviderError::Cancelled => WebCategory::Cancelled,
        ProviderError::RedirectDenied => WebCategory::RedirectDenied,
        ProviderError::UnexpectedContentType => WebCategory::UnexpectedContentType,
        ProviderError::AuthenticationOrEntitlement => WebCategory::AuthenticationOrEntitlement,
        ProviderError::RateLimited => WebCategory::RateLimited,
        ProviderError::Unavailable => WebCategory::Unavailable,
        ProviderError::RequestRejected => WebCategory::RequestRejected,
        ProviderError::ProviderExecutionFailed => WebCategory::ProviderExecutionFailed,
        ProviderError::MalformedCatalog => WebCategory::MalformedCatalog,
        ProviderError::MalformedResponse => WebCategory::MalformedResponse,
        ProviderError::ResponseLimitExceeded => WebCategory::ResponseLimitExceeded,
        ProviderError::IncompleteResponse => WebCategory::IncompleteResponse,
    };
    WebFailure { stage, category }
}
