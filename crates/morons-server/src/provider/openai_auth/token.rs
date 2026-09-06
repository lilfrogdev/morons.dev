use super::{MAX_TOKEN_LIFETIME_SECONDS, OAuthError, REFRESH_MARGIN_SECONDS, SCOPE};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Deserializer, de::Error as _};
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
impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = Zeroizing::new(String::deserialize(d)?);
        if text.is_empty()
            || text.len() > MAX_SECRET
            || !text.bytes().all(|b| (0x21..=0x7e).contains(&b))
        {
            return Err(D::Error::custom("invalid OAuth secret"));
        }
        Ok(Self(text))
    }
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
            if value.is_empty()
                || value.len() > MAX_SECRET
                || !value.bytes().all(|b| (0x21..=0x7e).contains(&b))
            {
                return Err(OAuthError::InvalidTokenResponse);
            }
        }
        let access = Secret(access);
        let (claimed, token_expiry) = account_claims(&access)?;
        if account.as_str() != claimed.0.as_str() || expires == 0 || expires > token_expiry {
            return Err(OAuthError::InvalidTokenResponse);
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenResponse {
    access_token: Secret,
    refresh_token: Secret,
    expires_in: u64,
    #[serde(default, deserialize_with = "present")]
    token_type: Option<String>,
    #[serde(default, deserialize_with = "present")]
    scope: Option<String>,
    #[serde(default, deserialize_with = "present")]
    id_token: Option<Secret>,
}
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

pub(super) fn parse_tokens(body: &[u8], now: u64) -> Result<OAuthTokens, OAuthError> {
    let invalid = OAuthError::InvalidTokenResponse;
    if body.len() > MAX_TOKEN_BODY {
        return Err(invalid);
    }
    let response: TokenResponse = serde_json::from_slice(body).map_err(|_| invalid)?;
    if response.expires_in <= REFRESH_MARGIN_SECONDS
        || response.expires_in > MAX_TOKEN_LIFETIME_SECONDS
        || response.token_type.as_ref().is_some_and(|s| s != "Bearer")
        || response.scope.as_ref().is_some_and(|s| !valid_scope(s))
    {
        return Err(invalid);
    }
    let expires = now.checked_add(response.expires_in).ok_or(invalid)?;
    let (account, token_expiry) = account_claims(&response.access_token)?;
    if token_expiry.saturating_sub(now) > MAX_TOKEN_LIFETIME_SECONDS {
        return Err(invalid);
    }
    let expires_at_seconds = expires.min(token_expiry);
    if expires_at_seconds.saturating_sub(now) <= REFRESH_MARGIN_SECONDS {
        return Err(invalid);
    }
    drop(response.id_token);
    Ok(OAuthTokens {
        access: response.access_token,
        refresh: response.refresh_token,
        account,
        expires_at_seconds,
    })
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

fn account_claims(token: &Secret) -> Result<(Secret, u64), OAuthError> {
    let invalid = OAuthError::InvalidTokenResponse;
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
    let claims =
        SecretJson(crate::provider::json::parse_strict_value(&decoded).map_err(|_| invalid)?);
    let object = claims.0.as_object().ok_or(invalid)?;
    let expires = object
        .get("exp")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0 && *n <= i64::MAX as u64)
        .ok_or(invalid)?;
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
        .ok_or(invalid)?;
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
