use super::*;
#[cfg(test)]
mod tests;
use crate::login_supervisor::credential_error;
impl ServerApplication {
    pub(super) async fn execute_openai_auth(
        &self,
        request: ApplicationRequest,
    ) -> Result<ApplicationOutcome, ApplicationError> {
        if self.stopping.load(Ordering::Acquire) || *self.shutdown_requests.borrow() {
            return Err(ApplicationError::ServiceUnavailable);
        }
        match request {
            ApplicationRequest::BeginOpenAiLogin {
                mutation_request_id,
                expected_generation,
            } => self
                .login_supervisor
                .start(mutation_request_id, expected_generation)
                .await
                .map(ApplicationOutcome::OpenAiLogin),
            ApplicationRequest::GetOpenAiCredentialStatus => {
                let status = self
                    .openai_credentials
                    .status()
                    .await
                    .map_err(credential_error)?;
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::OpenAiCredentialStatus {
                        credential: status_to_protocol(status),
                    },
                ))
            }
            ApplicationRequest::RemoveOpenAiCredential {
                mutation_request_id,
                expected_generation,
            } => {
                let status = self
                    .openai_credentials
                    .remove(
                        to_persistence_mutation_id(mutation_request_id),
                        expected_generation,
                    )
                    .await
                    .map_err(credential_error)?;
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::OpenAiCredentialStatus {
                        credential: morons_protocol::OpenAiCredentialStatus {
                            generation: status.generation,
                            state: morons_protocol::OpenAiCredentialState::Unconfigured,
                        },
                    },
                ))
            }
            _ => Err(ApplicationError::InvalidRequest),
        }
    }
}
fn status_to_protocol(
    status: crate::persistence::OpenAiCredentialStatus,
) -> morons_protocol::OpenAiCredentialStatus {
    use crate::persistence::OpenAiCredentialState as Stored;
    use morons_protocol::OpenAiCredentialState as Wire;
    morons_protocol::OpenAiCredentialStatus {
        generation: status.generation,
        state: match status.state {
            Stored::Unconfigured => Wire::Unconfigured,
            Stored::Configured => Wire::Configured,
            Stored::ReauthenticationRequired => Wire::ReauthenticationRequired,
        },
    }
}
