use crate::provider::{
    ProviderCancellationHandle,
    openai_auth::{OAuthError, OpenAiCredentialError, OpenAiCredentialProvider, OpenAiLogin},
    provider_cancellation,
};
use morons_protocol::{
    ApplicationError, MutationRequestId, OpenAiAuthorizationUrl, OpenAiLoginFailure as Failure,
    OpenAiLoginResult as Outcome,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex, Semaphore, watch},
    task::JoinHandle,
    time,
};

pub(crate) struct LoginSupervisor {
    credentials: Arc<OpenAiCredentialProvider>,
    slot: Arc<Semaphore>,
    active: Mutex<Option<Active>>,
    stopping: AtomicBool,
    installation_timeout: Duration,
    shutdown: watch::Sender<bool>,
    #[cfg(test)]
    install_started: tokio::sync::Notify,
}
struct Active {
    id: MutationRequestId,
    cancel: ProviderCancellationHandle,
    task: JoinHandle<()>,
}
pub(crate) struct LoginConnection {
    pub(crate) id: MutationRequestId,
    pub(crate) url: Option<OpenAiAuthorizationUrl>,
    supervisor: Arc<LoginSupervisor>,
    cancel: ProviderCancellationHandle,
    outcome: watch::Receiver<Option<Outcome>>,
}
impl Drop for LoginConnection {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
impl LoginConnection {
    pub(crate) fn cancel(&self, id: MutationRequestId) -> Result<(), ApplicationError> {
        if self.id != id {
            return Err(ApplicationError::InvalidRequest);
        }
        self.cancel.cancel();
        Ok(())
    }
    pub(crate) async fn finish(&mut self) -> Outcome {
        while self.outcome.borrow().is_none() {
            if self.outcome.changed().await.is_err() {
                break;
            }
        }
        self.supervisor.join(Some(self.id)).await;
        self.outcome.borrow().unwrap_or(Outcome::Failed {
            failure: Failure::InstallationUncertain,
        })
    }
}
impl LoginSupervisor {
    pub(crate) fn new(
        credentials: Arc<OpenAiCredentialProvider>,
        shutdown: watch::Sender<bool>,
    ) -> Arc<Self> {
        Arc::new(Self {
            credentials,
            slot: Arc::new(Semaphore::new(1)),
            active: Mutex::new(None),
            stopping: AtomicBool::new(false),
            installation_timeout: Duration::from_secs(60),
            shutdown,
            #[cfg(test)]
            install_started: tokio::sync::Notify::new(),
        })
    }
    pub(crate) async fn start(
        self: &Arc<Self>,
        id: MutationRequestId,
        generation: u64,
    ) -> Result<LoginConnection, ApplicationError> {
        self.start_with(id, generation, OpenAiLogin::begin()).await
    }
    async fn start_with(
        self: &Arc<Self>,
        id: MutationRequestId,
        generation: u64,
        login: impl std::future::Future<Output = Result<OpenAiLogin, OAuthError>>,
    ) -> Result<LoginConnection, ApplicationError> {
        let permit = self
            .slot
            .clone()
            .try_acquire_owned()
            .map_err(|_| failed(Failure::Busy))?;
        let mut active = self.active.try_lock().map_err(|_| failed(Failure::Busy))?;
        if active.as_ref().is_some_and(|a| !a.task.is_finished()) {
            return Err(failed(Failure::Busy));
        }
        if let Some(previous) = active.as_mut()
            && (&mut previous.task).await.is_err()
        {
            self.fatal();
        }
        active.take();
        if self.stopping.load(Ordering::Acquire) || *self.shutdown.borrow() {
            return Err(ApplicationError::ServiceUnavailable);
        }
        if id.as_bytes() == &[0; 16] {
            return Err(ApplicationError::InvalidRequest);
        }
        let status = time::timeout(Duration::from_secs(5), self.credentials.status())
            .await
            .map_err(|_| ApplicationError::ServiceUnavailable)?
            .map_err(credential_error)?;
        if status.generation != generation {
            return Err(ApplicationError::CredentialGenerationConflict);
        }
        if self.stopping.load(Ordering::Acquire) || *self.shutdown.borrow() {
            return Err(ApplicationError::ServiceUnavailable);
        }
        let login = time::timeout(Duration::from_secs(5), login)
            .await
            .map_err(|_| failed(Failure::Unavailable))?
            .map_err(|error| failed(oauth_failure(error)))?;
        if self.stopping.load(Ordering::Acquire) || *self.shutdown.borrow() {
            return Err(ApplicationError::ServiceUnavailable);
        }
        let url = OpenAiAuthorizationUrl::new(login.authorization_url().as_str().to_owned())
            .map_err(|_| ApplicationError::Internal)?;
        let (cancel, mut cancellation) = provider_cancellation();
        let (outcomes, outcome) = watch::channel(None);
        let supervisor = self.clone();
        let task = tokio::spawn(async move {
            let _permit = permit;
            let result = match login.complete(&mut cancellation).await {
                Ok(tokens) if !cancellation.is_cancelled() => {
                    // Installation may commit after cancellation; never label that as rollback.
                    #[cfg(test)]
                    supervisor.install_started.notify_one();
                    match time::timeout(
                        supervisor.installation_timeout,
                        supervisor.credentials.install(
                            crate::persistence::MutationRequestId::from_bytes(*id.as_bytes()),
                            generation,
                            tokens,
                        ),
                    )
                    .await
                    {
                        Ok(Ok(status)) => Outcome::Installed {
                            generation: status.generation,
                        },
                        Ok(Err(OpenAiCredentialError::Persistence(
                            crate::persistence::PersistenceError::CredentialGenerationConflict,
                        ))) => Outcome::Failed {
                            failure: Failure::CredentialChanged,
                        },
                        Ok(Err(OpenAiCredentialError::Persistence(
                            crate::persistence::PersistenceError::RequestConflict
                            | crate::persistence::PersistenceError::CredentialMutationNotApplied,
                        ))) => Outcome::Failed {
                            failure: Failure::Unavailable,
                        },
                        _ => {
                            supervisor.fatal();
                            Outcome::Failed {
                                failure: Failure::InstallationUncertain,
                            }
                        }
                    }
                }
                Ok(_) | Err(OAuthError::Cancelled) => Outcome::CancelledBeforeInstallation,
                Err(error) => Outcome::Failed {
                    failure: oauth_failure(error),
                },
            };
            outcomes.send_replace(Some(result));
        });
        *active = Some(Active {
            id,
            cancel: cancel.clone(),
            task,
        });
        Ok(LoginConnection {
            id,
            url: Some(url),
            supervisor: self.clone(),
            cancel,
            outcome,
        })
    }
    async fn join(&self, id: Option<MutationRequestId>) {
        let mut active = self.active.lock().await;
        if let Some(a) = active.as_mut().filter(|a| id.is_none_or(|id| a.id == id)) {
            if id.is_none() {
                a.cancel.cancel();
            }
            if (&mut a.task).await.is_err() {
                self.fatal();
            }
            active.take();
        }
    }
    pub(crate) async fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        self.join(None).await;
    }
    fn fatal(&self) {
        self.stopping.store(true, Ordering::Release);
        self.shutdown.send_replace(true);
    }
}
pub(crate) fn credential_error(error: OpenAiCredentialError) -> ApplicationError {
    match error {
        OpenAiCredentialError::Persistence(error) => {
            crate::application::conversions::to_application_error(error)
        }
        _ => ApplicationError::ServiceUnavailable,
    }
}
fn failed(failure: Failure) -> ApplicationError {
    ApplicationError::OpenAiLoginFailed { failure }
}
fn oauth_failure(error: OAuthError) -> Failure {
    match error {
        OAuthError::Busy => Failure::Busy,
        OAuthError::CallbackUnavailable => Failure::CallbackUnavailable,
        OAuthError::AuthorizationDenied => Failure::Denied,
        OAuthError::Deadline => Failure::Expired,
        OAuthError::TokenRejected => Failure::ExchangeRejected,
        OAuthError::ExchangeUncertain => Failure::ExchangeUncertain,
        OAuthError::InvalidTokenResponse => Failure::InvalidResponse,
        OAuthError::CallbackLimit | OAuthError::EntropyUnavailable | OAuthError::Cancelled => {
            Failure::Unavailable
        }
    }
}
#[cfg(test)]
mod tests;
