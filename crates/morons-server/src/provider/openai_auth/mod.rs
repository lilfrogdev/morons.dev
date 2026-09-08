//! Server-side OAuth primitives; not an IPC surface or a credential-store replacement.
mod callback;
mod credentials;
mod failure;
mod token;
mod transport;

use std::{
    fmt,
    sync::{Arc, LazyLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest as _, Sha256};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::{self, Instant},
};
use zeroize::Zeroizing;

use super::ProviderCancellation;
use callback::Callback;
pub use credentials::{OpenAiCredentialError, OpenAiCredentialLease, OpenAiCredentialProvider};
pub use failure::TokenResponseFailure;
pub use token::OAuthTokens;
pub(crate) use token::{OAuthRefreshGrant, OpenAiAuthorization};
use transport::TokenClient;

const AUTHORIZE_URI: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URI: &str = "https://auth.openai.com/oauth/token";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const SCOPE: &str = "openid profile email offline_access";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(600);
const REFRESH_MARGIN_SECONDS: u64 = 300;
const MAX_TOKEN_LIFETIME_SECONDS: u64 = 30 * 24 * 60 * 60;
static LOGIN_SLOT: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(1)));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthError {
    Busy,
    EntropyUnavailable,
    CallbackUnavailable,
    CallbackLimit,
    AuthorizationDenied,
    Cancelled,
    Deadline,
    TokenRejected,
    ExchangeUncertain,
    InvalidTokenResponse(TokenResponseFailure),
}

impl fmt::Display for OAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Busy => "an OpenAI login is already in progress",
            Self::EntropyUnavailable => "OAuth entropy is unavailable",
            Self::CallbackUnavailable => "the OpenAI loopback callback is unavailable",
            Self::CallbackLimit => "the OpenAI callback request limit was reached",
            Self::AuthorizationDenied => "OpenAI authorization was denied",
            Self::Cancelled => "OpenAI login was cancelled",
            Self::Deadline => "OpenAI login expired",
            Self::TokenRejected => "OpenAI rejected the token exchange; start a new login",
            Self::ExchangeUncertain => "the OpenAI token exchange is uncertain; do not retry it",
            Self::InvalidTokenResponse(reason) => {
                return write!(
                    f,
                    "the OpenAI token response was rejected ({reason}); nothing was retried"
                );
            }
        })
    }
}
impl std::error::Error for OAuthError {}

/// Ephemeral browser interaction data. Never put this in ordinary status, logs or history.
pub struct AuthorizationUrl(Zeroizing<String>);
impl AuthorizationUrl {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for AuthorizationUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthorizationUrl([REDACTED])")
    }
}

pub struct OpenAiLogin {
    callback: Callback,
    verifier: Zeroizing<String>,
    state: Zeroizing<String>,
    url: AuthorizationUrl,
    redirect: String,
    client: TokenClient,
    deadline: Instant,
    _permit: OwnedSemaphorePermit,
}

impl fmt::Debug for OpenAiLogin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OpenAiLogin([REDACTED])")
    }
}

impl OpenAiLogin {
    /// Binds the fixed callback before exposing the URL; does not launch a browser or send HTTP.
    pub async fn begin() -> Result<Self, OAuthError> {
        let permit = LOGIN_SLOT
            .clone()
            .try_acquire_owned()
            .map_err(|_| OAuthError::Busy)?;
        let deadline = Instant::now() + LOGIN_TIMEOUT;
        let callback = Callback::bind(
            "127.0.0.1:1455".parse().expect("fixed callback address"),
            "localhost:1455".to_owned(),
        )
        .await?;
        Self::create(
            callback,
            REDIRECT_URI.to_owned(),
            TokenClient::new(),
            permit,
            deadline,
        )
    }

    fn create(
        callback: Callback,
        redirect: String,
        client: TokenClient,
        permit: OwnedSemaphorePermit,
        deadline: Instant,
    ) -> Result<Self, OAuthError> {
        let verifier = random_value()?;
        let state = random_value()?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let query = form(&[
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", &redirect),
            ("scope", SCOPE),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("state", &state),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("originator", "morons"),
        ]);
        let url = AuthorizationUrl(Zeroizing::new(format!(
            "{AUTHORIZE_URI}?{}",
            query.as_str()
        )));
        Ok(Self {
            callback,
            verifier,
            state,
            url,
            redirect,
            client,
            deadline,
            _permit: permit,
        })
    }

    pub fn authorization_url(&self) -> &AuthorizationUrl {
        &self.url
    }

    /// Poll promptly under owned supervision; dropping this future closes the callback.
    /// Caller must durably install the returned material before reporting login.
    pub async fn complete(
        self,
        cancellation: &mut ProviderCancellation,
    ) -> Result<OAuthTokens, OAuthError> {
        if cancellation.is_cancelled() {
            return Err(OAuthError::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(OAuthError::Deadline);
        }
        let deadline = self.deadline;
        let dispatch_cancellation = cancellation.clone();
        let result = tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(OAuthError::Cancelled),
            result = time::timeout_at(deadline, self.complete_inner(&dispatch_cancellation)) => {
                result.map_err(|_| OAuthError::Deadline)?
            }
        };
        if cancellation.is_cancelled() {
            Err(OAuthError::Cancelled)
        } else if Instant::now() >= deadline {
            Err(OAuthError::Deadline)
        } else {
            result
        }
    }

    async fn complete_inner(
        self,
        cancellation: &ProviderCancellation,
    ) -> Result<OAuthTokens, OAuthError> {
        let code = self.callback.receive(&self.state).await?;
        if cancellation.is_cancelled() {
            return Err(OAuthError::Cancelled);
        }
        let body = form(&[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", &code),
            ("code_verifier", &self.verifier),
            ("redirect_uri", &self.redirect),
        ]);
        if cancellation.is_cancelled() {
            return Err(OAuthError::Cancelled);
        }
        self.client.exchange(body).await
    }
}

fn random_value() -> Result<Zeroizing<String>, OAuthError> {
    let mut bytes = Zeroizing::new([0_u8; 32]);
    getrandom::fill(bytes.as_mut()).map_err(|_| OAuthError::EntropyUnavailable)?;
    Ok(Zeroizing::new(URL_SAFE_NO_PAD.encode(bytes.as_slice())))
}

fn form(fields: &[(&str, &str)]) -> Zeroizing<String> {
    let capacity = fields
        .iter()
        .map(|(key, value)| 3 * (key.len() + value.len()) + 2)
        .sum();
    let mut out = Zeroizing::new(String::with_capacity(capacity));
    for (index, (key, value)) in fields.iter().enumerate() {
        if index != 0 {
            out.push('&');
        }
        encode_component(&mut out, key);
        out.push('=');
        encode_component(&mut out, value);
    }
    out
}

fn encode_component(out: &mut String, value: &str) {
    const HEX: &[u8] = b"0123456789ABCDEF";
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(b))
            }
            _ => {
                out.push('%');
                out.push(char::from(HEX[usize::from(b >> 4)]));
                out.push(char::from(HEX[usize::from(b & 15)]));
            }
        }
    }
}

fn now_seconds() -> Result<u64, OAuthError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .map_err(|_| OAuthError::InvalidTokenResponse(TokenResponseFailure::Clock))
}

#[cfg(test)]
pub(crate) mod tests;
