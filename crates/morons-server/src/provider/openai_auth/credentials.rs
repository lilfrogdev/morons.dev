use super::{CLIENT_ID, TokenClient, form};
use crate::{
    persistence::{PersistenceError, PreparedOpenAiCredential, SessionStore},
    provider::ProviderCancellation,
};
use std::{fmt, sync::Arc, time::Duration};
use tokio::time::{self, Instant};

#[derive(Debug)]
pub enum OpenAiCredentialError {
    Persistence(PersistenceError),
    Cancelled,
    Deadline,
}
impl fmt::Display for OpenAiCredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Persistence(_) => "OpenAI credential access failed",
            Self::Cancelled => "OpenAI credential access was cancelled",
            Self::Deadline => "OpenAI credential access timed out",
        })
    }
}
impl std::error::Error for OpenAiCredentialError {}
impl From<PersistenceError> for OpenAiCredentialError {
    fn from(value: PersistenceError) -> Self {
        Self::Persistence(value)
    }
}

pub struct OpenAiCredentialProvider {
    sessions: Arc<SessionStore>,
    client: TokenClient,
}
pub struct OpenAiCredentialLease<'a>(PreparedOpenAiCredential<'a>);
impl fmt::Debug for OpenAiCredentialLease<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OpenAiCredentialLease([REDACTED])")
    }
}
impl OpenAiCredentialLease<'_> {
    /// For trusted server dispatch only; never forward these values over application IPC.
    pub fn authorization_headers(&self) -> http::HeaderMap {
        self.0
            .headers()
            .expect("a resolved credential lease has authorization")
    }
}
impl OpenAiCredentialProvider {
    pub(crate) fn new(sessions: Arc<SessionStore>) -> Self {
        Self {
            sessions,
            client: TokenClient::new(),
        }
    }
    #[cfg(test)]
    pub(crate) fn for_test(sessions: Arc<SessionStore>, uri: http::Uri) -> Self {
        Self {
            sessions,
            client: TokenClient::for_test(uri),
        }
    }

    pub async fn install(
        &self,
        request: crate::persistence::MutationRequestId,
        generation: u64,
        tokens: super::OAuthTokens,
    ) -> Result<crate::persistence::CredentialIdentityStatus, OpenAiCredentialError> {
        self.sessions
            .set_openai_credential(request, generation, tokens)
            .await
            .map_err(Into::into)
    }
    pub async fn remove(
        &self,
        request: crate::persistence::MutationRequestId,
        generation: u64,
    ) -> Result<crate::persistence::CredentialIdentityStatus, OpenAiCredentialError> {
        self.sessions
            .remove_openai_credential(request, generation)
            .await
            .map_err(Into::into)
    }
    pub async fn status(
        &self,
    ) -> Result<crate::persistence::OpenAiCredentialStatus, OpenAiCredentialError> {
        self.sessions
            .openai_credential_status()
            .await
            .map_err(Into::into)
    }

    pub async fn lease(
        &self,
        generation: u64,
        cancellation: &mut ProviderCancellation,
    ) -> Result<OpenAiCredentialLease<'_>, OpenAiCredentialError> {
        let deadline = Instant::now() + Duration::from_secs(60);
        if cancellation.is_cancelled() {
            return Err(OpenAiCredentialError::Cancelled);
        }
        let mut lease = tokio::select! {
            biased;
            ()=cancellation.cancelled()=>return Err(OpenAiCredentialError::Cancelled),
            result=time::timeout_at(deadline,self.sessions.begin_openai_access(generation))=>result.map_err(|_|OpenAiCredentialError::Deadline)??,
        };
        if let Some(grant) = lease.take_refresh() {
            let body = form(&[
                ("grant_type", "refresh_token"),
                ("client_id", CLIENT_ID),
                ("refresh_token", grant.token()),
            ]);
            let result = tokio::select! {
                biased;
                ()=cancellation.cancelled()=>Err(OpenAiCredentialError::Cancelled),
                result=time::timeout_at(deadline,self.client.exchange(body))=>result.map_err(|_|OpenAiCredentialError::Deadline),
            };
            let tokens = match result {
                Ok(tokens) => tokens.ok(),
                Err(error) => {
                    let _ = time::timeout(Duration::from_secs(5), lease.finish_refresh(None)).await;
                    return Err(error);
                }
            };
            tokio::select! {
                biased;
                ()=cancellation.cancelled()=>return Err(OpenAiCredentialError::Cancelled),
                result=time::timeout_at(deadline,lease.finish_refresh(tokens))=>result.map_err(|_|OpenAiCredentialError::Deadline)??,
            }
        }
        if cancellation.is_cancelled() {
            return Err(OpenAiCredentialError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(OpenAiCredentialError::Deadline);
        }
        Ok(OpenAiCredentialLease(lease))
    }
}
