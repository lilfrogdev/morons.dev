mod envelope;

use super::{OAuthError, TokenResponseFailure as Reason};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
pub(super) use envelope::parse_tokens;
use serde_json::Value;
use std::fmt;
use zeroize::{Zeroize as _, Zeroizing};

const MAX_SECRET: usize = 16 * 1024;
const MAX_TOKEN_BODY: usize = 64 * 1024;

pub(super) struct Secret(Zeroizing<String>);
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}
fn valid_secret(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_SECRET && text.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

/// Opaque server-owned material. Never serialize this as an application response.
pub struct OAuthTokens {
    access: Secret,
    refresh: Secret,
    account: Secret,
    expires_at_seconds: u64,
}
impl OAuthTokens {
    pub fn expires_at_seconds(&self) -> u64 {
        self.expires_at_seconds
    }

    pub(crate) fn stored_parts(&self) -> (&str, &str, &str, u64) {
        (
            &self.access.0,
            &self.refresh.0,
            &self.account.0,
            self.expires_at_seconds,
        )
    }

    pub(crate) fn from_stored(
        access: Zeroizing<String>,
        refresh: Zeroizing<String>,
        account: Zeroizing<String>,
        expires: u64,
    ) -> Result<Self, OAuthError> {
        for value in [&access, &refresh] {
            if !valid_secret(value) {
                return Err(OAuthError::InvalidTokenResponse(Reason::StoredCredential));
            }
        }
        let access = Secret(access);
        let (claimed, token_expiry) = account_claims(&access)?;
        if account.as_str() != claimed.0.as_str() || expires == 0 || expires > token_expiry {
            return Err(OAuthError::InvalidTokenResponse(Reason::StoredCredential));
        }
        Ok(Self {
            access,
            refresh: Secret(refresh),
            account: claimed,
            expires_at_seconds: expires,
        })
    }

    #[cfg(test)]
    pub(crate) fn fixture(account: &str, refresh: &str, expires: u64) -> Self {
        let claims = serde_json::json!({"exp":expires.max(super::now_seconds().unwrap()+7200),"https://api.openai.com/auth":{"chatgpt_account_id":account}});
        let access = format!(
            "header.{}.signature",
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        Self::from_stored(
            Zeroizing::new(access),
            Zeroizing::new(refresh.to_owned()),
            Zeroizing::new(account.to_owned()),
            expires,
        )
        .unwrap()
    }
    pub(crate) fn same_account(&self, other: &Self) -> bool {
        self.account.0 == other.account.0
    }
    pub(crate) fn refresh_grant(&self) -> OAuthRefreshGrant {
        OAuthRefreshGrant {
            refresh: Secret(self.refresh.0.clone()),
        }
    }
    pub(crate) fn authorization(&self) -> OpenAiAuthorization {
        OpenAiAuthorization {
            access: Secret(self.access.0.clone()),
            account: Secret(self.account.0.clone()),
        }
    }
}

#[derive(Debug)]
pub(crate) struct OAuthRefreshGrant {
    pub(super) refresh: Secret,
}
impl OAuthRefreshGrant {
    pub(super) fn token(&self) -> &str {
        &self.refresh.0
    }
}

#[derive(Debug)]
pub(crate) struct OpenAiAuthorization {
    access: Secret,
    account: Secret,
}
impl OpenAiAuthorization {
    pub(crate) fn headers(&self) -> http::HeaderMap {
        let mut bearer = Zeroizing::new(String::with_capacity(7 + self.access.0.len()));
        bearer.push_str("Bearer ");
        bearer.push_str(&self.access.0);
        let mut token =
            http::HeaderValue::from_str(&bearer).expect("validated visible ASCII token");
        token.set_sensitive(true);
        let mut account =
            http::HeaderValue::from_str(&self.account.0).expect("validated account header");
        account.set_sensitive(true);
        let mut headers = http::HeaderMap::new();
        headers.insert(http::header::AUTHORIZATION, token);
        headers.insert("chatgpt-account-id", account);
        headers
    }
}
impl fmt::Debug for OAuthTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthTokens")
            .field("access", &self.access)
            .field("refresh", &self.refresh)
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

fn account_claims(token: &Secret) -> Result<(Secret, u64), OAuthError> {
    let invalid = OAuthError::InvalidTokenResponse(Reason::AccessTokenFormat);
    let mut parts = token.0.split('.');
    let header = parts.next().ok_or(invalid)?;
    let payload = parts.next().ok_or(invalid)?;
    let signature = parts.next().ok_or(invalid)?;
    if parts.next().is_some()
        || [header, payload, signature].iter().any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
        })
    {
        return Err(invalid);
    }
    let mut decoded = Zeroizing::new(vec![0_u8; payload.len()]);
    let length = URL_SAFE_NO_PAD
        .decode_slice(payload, &mut decoded)
        .map_err(|_| invalid)?;
    decoded.truncate(length);
    let claims = SecretJson(
        crate::provider::json::parse_strict_value(&decoded)
            .map_err(|_| OAuthError::InvalidTokenResponse(Reason::ClaimsJson))?,
    );
    let object = claims
        .0
        .as_object()
        .ok_or(OAuthError::InvalidTokenResponse(Reason::ClaimsJson))?;
    let expires = object
        .get("exp")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0 && *n <= i64::MAX as u64)
        .ok_or(OAuthError::InvalidTokenResponse(Reason::ClaimExpiry))?;
    let account = object
        .get("https://api.openai.com/auth")
        .and_then(Value::as_object)
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
        })
        .ok_or(OAuthError::InvalidTokenResponse(Reason::AccountClaim))?;
    Ok((Secret(Zeroizing::new(account.to_owned())), expires))
}

struct SecretJson(Value);
impl Drop for SecretJson {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}
fn wipe(value: &mut Value) {
    match value {
        Value::String(s) => s.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(wipe),
        Value::Object(values) => {
            for (mut key, mut value) in std::mem::take(values) {
                key.zeroize();
                wipe(&mut value);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
pub(super) mod tests;
