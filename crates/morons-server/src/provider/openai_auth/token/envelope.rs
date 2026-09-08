use super::{MAX_TOKEN_BODY, OAuthTokens, Secret, SecretJson, account_claims, valid_secret};
use crate::provider::openai_auth::{
    MAX_TOKEN_LIFETIME_SECONDS, OAuthError, REFRESH_MARGIN_SECONDS, SCOPE,
    TokenResponseFailure as Reason,
};
use serde_json::Value;
use zeroize::Zeroizing;

pub(in crate::provider::openai_auth) fn parse_tokens(
    body: &[u8],
    now: u64,
) -> Result<OAuthTokens, OAuthError> {
    use OAuthError::InvalidTokenResponse as invalid;
    if body.len() > MAX_TOKEN_BODY {
        return Err(invalid(Reason::BodyBounds));
    }
    // Strict even for discarded extensions: never let duplicates hide a known field.
    // The owned tree wipes strings/keys on every post-parse return, including extensions.
    let response = SecretJson(
        crate::provider::json::parse_strict_value(body).map_err(|_| invalid(Reason::Json))?,
    );
    let fields = response.0.as_object().ok_or(invalid(Reason::TokenFields))?;
    if fields.len() > 32
        || fields.keys().any(|key| key.is_empty() || key.len() > 128)
        || ["error", "error_description", "error_uri"]
            .iter()
            .any(|key| fields.contains_key(*key))
    {
        return Err(invalid(Reason::TokenFields));
    }
    let access = secret(fields.get("access_token"))?;
    let refresh = secret(fields.get("refresh_token"))?;
    if let Some(value) = fields.get("id_token")
        && value.as_str().is_none_or(|s| !valid_secret(s))
    {
        return Err(invalid(Reason::TokenFields));
    }
    if let Some(value) = fields.get("token_type")
        && value
            .as_str()
            .is_none_or(|s| !s.eq_ignore_ascii_case("Bearer"))
    {
        return Err(invalid(Reason::TokenType));
    }
    if let Some(value) = fields.get("scope")
        && value.as_str().is_none_or(|s| !valid_scope(s))
    {
        return Err(invalid(Reason::Scope));
    }
    let lifetime = fields
        .get("expires_in")
        .and_then(Value::as_u64)
        .filter(|n| *n > REFRESH_MARGIN_SECONDS && *n <= MAX_TOKEN_LIFETIME_SECONDS)
        .ok_or(invalid(Reason::ResponseLifetime))?;
    let expires = now
        .checked_add(lifetime)
        .ok_or(invalid(Reason::ResponseLifetime))?;
    let (account, token_expiry) = account_claims(&access)?;
    if token_expiry.saturating_sub(now) > MAX_TOKEN_LIFETIME_SECONDS {
        return Err(invalid(Reason::EffectiveLifetime));
    }
    let expires_at_seconds = expires.min(token_expiry);
    if expires_at_seconds.saturating_sub(now) <= REFRESH_MARGIN_SECONDS {
        return Err(invalid(Reason::EffectiveLifetime));
    }
    // No extension can supply tokens, account, expiry, scope, origin or policy.
    Ok(OAuthTokens {
        access,
        refresh,
        account,
        expires_at_seconds,
    })
}

fn secret(value: Option<&Value>) -> Result<Secret, OAuthError> {
    value
        .and_then(Value::as_str)
        .filter(|text| valid_secret(text))
        .map(|text| Secret(Zeroizing::new(text.to_owned())))
        .ok_or(OAuthError::InvalidTokenResponse(Reason::TokenFields))
}

fn valid_scope(scope: &str) -> bool {
    if scope.len() > 128 {
        return false;
    }
    let mut words: Vec<_> = scope.split(' ').collect();
    words.sort_unstable();
    let mut expected: Vec<_> = SCOPE.split(' ').collect();
    expected.sort_unstable();
    words == expected
}
