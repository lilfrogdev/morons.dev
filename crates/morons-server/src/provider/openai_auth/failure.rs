use std::fmt;

/// Closed validation stages only. Never attach provider data or parser errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenResponseFailure {
    Headers,
    BodyBounds,
    BodyFraming,
    Json,
    TokenFields,
    TokenType,
    Scope,
    ResponseLifetime,
    AccessTokenFormat,
    ClaimsJson,
    ClaimExpiry,
    AccountClaim,
    EffectiveLifetime,
    Clock,
    StoredCredential,
}

impl fmt::Display for TokenResponseFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Headers => "headers",
            Self::BodyBounds => "body-bounds",
            Self::BodyFraming => "body-framing",
            Self::Json => "json",
            Self::TokenFields => "token-fields",
            Self::TokenType => "token-type",
            Self::Scope => "scope",
            Self::ResponseLifetime => "response-lifetime",
            Self::AccessTokenFormat => "access-token-format",
            Self::ClaimsJson => "claims-json",
            Self::ClaimExpiry => "claim-expiry",
            Self::AccountClaim => "account-claim",
            Self::EffectiveLifetime => "effective-lifetime",
            Self::Clock => "clock",
            Self::StoredCredential => "stored-credential",
        })
    }
}
