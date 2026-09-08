use super::{
    Backend, CredentialIdentityStatus, MutationRequestId, OpenAiCredentialStatus, PersistenceError,
    SessionStore, WorkerRequest, credentials::openai::OpenAiAccess,
};
use crate::provider::openai_auth::{OAuthRefreshGrant, OAuthTokens, OpenAiAuthorization};
use std::fmt;
use tokio::sync::{MutexGuard, oneshot};

pub(crate) struct PreparedOpenAiCredential<'a> {
    store: &'a SessionStore,
    generation: u64,
    marker: Option<[u8; 16]>,
    grant: Option<OAuthRefreshGrant>,
    authorization: Option<OpenAiAuthorization>,
    _guard: MutexGuard<'a, ()>,
}
impl fmt::Debug for PreparedOpenAiCredential<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PreparedOpenAiCredential([REDACTED])")
    }
}
impl PreparedOpenAiCredential<'_> {
    pub(crate) fn take_refresh(&mut self) -> Option<OAuthRefreshGrant> {
        self.grant.take()
    }
    pub(crate) fn headers(&self) -> Result<http::HeaderMap, PersistenceError> {
        self.authorization
            .as_ref()
            .map(OpenAiAuthorization::headers)
            .ok_or(PersistenceError::CredentialReauthenticationRequired)
    }
    pub(crate) async fn finish_refresh(
        &mut self,
        tokens: Option<OAuthTokens>,
    ) -> Result<(), PersistenceError> {
        let marker = self.marker.take().ok_or(PersistenceError::InvalidState {
            reason: "OpenAI refresh ownership was already consumed",
        })?;
        let (tx, rx) = oneshot::channel();
        self.store
            .send_openai(OpenAiWorkerRequest::Finish {
                generation: self.generation,
                marker,
                tokens,
                response: tx,
            })
            .await?;
        self.authorization = Some(rx.await.map_err(|_| PersistenceError::WorkerStopped)??);
        Ok(())
    }
}
impl SessionStore {
    pub async fn openai_credential_status(
        &self,
    ) -> Result<OpenAiCredentialStatus, PersistenceError> {
        let (tx, rx) = oneshot::channel();
        self.send_openai(OpenAiWorkerRequest::Status(tx)).await?;
        rx.await.map_err(|_| PersistenceError::WorkerStopped)?
    }
    pub async fn set_openai_credential(
        &self,
        request: MutationRequestId,
        generation: u64,
        tokens: OAuthTokens,
    ) -> Result<CredentialIdentityStatus, PersistenceError> {
        self.mutate_openai(request, generation, Some(tokens)).await
    }
    pub async fn remove_openai_credential(
        &self,
        request: MutationRequestId,
        generation: u64,
    ) -> Result<CredentialIdentityStatus, PersistenceError> {
        self.mutate_openai(request, generation, None).await
    }
    async fn mutate_openai(
        &self,
        request: MutationRequestId,
        generation: u64,
        tokens: Option<OAuthTokens>,
    ) -> Result<CredentialIdentityStatus, PersistenceError> {
        if request.is_zero() {
            return Err(PersistenceError::InvalidInput {
                reason: "a mutation request identifier must not be all zeroes",
            });
        }
        let _guard = self.openai_dispatch_lock.lock().await;
        let (tx, rx) = oneshot::channel();
        self.send_openai(OpenAiWorkerRequest::Mutate {
            request,
            generation,
            tokens,
            response: tx,
        })
        .await?;
        rx.await.map_err(|_| PersistenceError::WorkerStopped)?
    }
    pub(crate) async fn begin_openai_access(
        &self,
        generation: u64,
    ) -> Result<PreparedOpenAiCredential<'_>, PersistenceError> {
        let guard = self.openai_dispatch_lock.lock().await;
        let (tx, rx) = oneshot::channel();
        self.send_openai(OpenAiWorkerRequest::Begin {
            generation,
            response: tx,
        })
        .await?;
        let access = rx.await.map_err(|_| PersistenceError::WorkerStopped)??;
        let (authorization, marker, grant) = match access {
            OpenAiAccess::Fresh(auth) => (Some(auth), None, None),
            OpenAiAccess::Refresh { marker, grant } => (None, Some(marker), Some(grant)),
        };
        Ok(PreparedOpenAiCredential {
            store: self,
            generation,
            authorization,
            marker,
            grant,
            _guard: guard,
        })
    }
    async fn send_openai(&self, request: OpenAiWorkerRequest) -> Result<(), PersistenceError> {
        self.sender()?
            .send(WorkerRequest::OpenAi(request))
            .await
            .map_err(|_| PersistenceError::WorkerStopped)
    }
}

pub(super) enum OpenAiWorkerRequest {
    Status(oneshot::Sender<Result<OpenAiCredentialStatus, PersistenceError>>),
    Mutate {
        request: MutationRequestId,
        generation: u64,
        tokens: Option<OAuthTokens>,
        response: oneshot::Sender<Result<CredentialIdentityStatus, PersistenceError>>,
    },
    Begin {
        generation: u64,
        response: oneshot::Sender<Result<OpenAiAccess, PersistenceError>>,
    },
    Finish {
        generation: u64,
        marker: [u8; 16],
        tokens: Option<OAuthTokens>,
        response: oneshot::Sender<Result<OpenAiAuthorization, PersistenceError>>,
    },
}
impl OpenAiWorkerRequest {
    pub(super) fn execute(self, backend: &mut Backend) {
        match self {
            Self::Status(response) => {
                let _ = response.send(backend.openai_credential_status());
            }
            Self::Mutate {
                request,
                generation,
                tokens,
                response,
            } => {
                let result = match tokens {
                    Some(tokens) => backend.set_openai_credential(request, generation, tokens),
                    None => backend.remove_openai_credential(request, generation),
                };
                let _ = response.send(result);
            }
            Self::Begin {
                generation,
                response,
            } => {
                let result = backend.begin_openai_access(generation);
                let _ = response.send(result);
            }
            Self::Finish {
                generation,
                marker,
                tokens,
                response,
            } => {
                let result = backend.finish_openai_refresh(generation, marker, tokens);
                let _ = response.send(result);
            }
        }
    }
}
