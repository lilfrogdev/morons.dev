use super::Backend;
use crate::persistence::{
    CredentialIdentityStatus, OpenAiCredentialState, PersistenceError, RunService,
};

impl Backend {
    pub(super) fn model_credential_status(
        &mut self,
        service: RunService,
    ) -> Result<CredentialIdentityStatus, PersistenceError> {
        match service {
            RunService::Zen | RunService::Go => {
                let status = self.open_code_credential_status()?;
                Ok(CredentialIdentityStatus {
                    configured: status.configured,
                    generation: status.generation,
                })
            }
            RunService::OpenAiChatGpt => {
                let status = self.openai_credential_status()?;
                Ok(CredentialIdentityStatus {
                    configured: status.state == OpenAiCredentialState::Configured,
                    generation: status.generation,
                })
            }
        }
    }
    pub(super) fn require_model_credential(
        &mut self,
        service: RunService,
    ) -> Result<u64, PersistenceError> {
        if service == RunService::OpenAiChatGpt {
            let status = self.openai_credential_status()?;
            return match status.state {
                OpenAiCredentialState::Configured => Ok(status.generation),
                OpenAiCredentialState::Unconfigured => {
                    Err(PersistenceError::OpenAiCredentialNotConfigured)
                }
                OpenAiCredentialState::ReauthenticationRequired => {
                    Err(PersistenceError::CredentialReauthenticationRequired)
                }
            };
        }
        let status = self.model_credential_status(service)?;
        if !status.configured {
            return Err(PersistenceError::CredentialNotConfigured);
        }
        Ok(status.generation)
    }
}
