use super::*;
use crate::{
    persistence::{CredentialIdentityStatus, CredentialKind, OpenAiCredentialStatus},
    provider::openai_auth::OAuthTokens,
};

pub(super) enum CredentialUpdate {
    OpenCode(Option<StoredOpenCodeApiKey>),
    OpenAi(Option<OAuthTokens>),
}
impl CredentialUpdate {
    pub fn kind(&self) -> CredentialKind {
        match self {
            Self::OpenCode(_) => CredentialKind::OpenCode,
            Self::OpenAi(_) => CredentialKind::OpenAiChatGpt,
        }
    }
    pub fn operation(&self) -> i64 {
        let configured = match self {
            Self::OpenCode(value) => value.is_some(),
            Self::OpenAi(value) => value.is_some(),
        };
        if configured {
            MUTATION_OPERATION_CREDENTIAL_SET
        } else {
            MUTATION_OPERATION_CREDENTIAL_REMOVE
        }
    }
}
impl Backend {
    pub(crate) fn openai_credential_status(
        &mut self,
    ) -> Result<OpenAiCredentialStatus, PersistenceError> {
        self.recover_credential_mutations()?;
        Ok(self.openai_credentials.status())
    }
    pub(crate) fn set_openai_credential(
        &mut self,
        request: MutationRequestId,
        generation: u64,
        tokens: OAuthTokens,
    ) -> Result<CredentialIdentityStatus, PersistenceError> {
        self.mutate_credential(request, generation, CredentialUpdate::OpenAi(Some(tokens)))
    }
    pub(crate) fn remove_openai_credential(
        &mut self,
        request: MutationRequestId,
        generation: u64,
    ) -> Result<CredentialIdentityStatus, PersistenceError> {
        self.mutate_credential(request, generation, CredentialUpdate::OpenAi(None))
    }
    pub(crate) fn begin_openai_access(
        &mut self,
        generation: u64,
    ) -> Result<crate::persistence::credentials::openai::OpenAiAccess, PersistenceError> {
        self.openai_credential_status()?;
        self.openai_credentials.begin_access(
            generation,
            random_identifier()?,
            current_time_milliseconds()? / 1000,
        )
    }
    pub(crate) fn finish_openai_refresh(
        &mut self,
        generation: u64,
        marker: [u8; 16],
        tokens: Option<OAuthTokens>,
    ) -> Result<crate::provider::openai_auth::OpenAiAuthorization, PersistenceError> {
        self.openai_credential_status()?;
        self.openai_credentials.finish_refresh(
            generation,
            marker,
            tokens,
            current_time_milliseconds()? / 1000,
        )
    }
    pub(super) fn credential_identity(&self, kind: CredentialKind) -> CredentialIdentityStatus {
        match kind {
            CredentialKind::OpenCode => self.credentials.status().into(),
            CredentialKind::OpenAiChatGpt => self.openai_credentials.identity(),
        }
    }
    pub(super) fn credential_marker(&self, kind: CredentialKind) -> &[u8; 16] {
        match kind {
            CredentialKind::OpenCode => self.credentials.state().mutation_marker(),
            CredentialKind::OpenAiChatGpt => self.openai_credentials.marker(),
        }
    }
    pub(super) fn credential_consistent(&self, kind: CredentialKind) -> bool {
        match kind {
            CredentialKind::OpenCode => self.credentials.is_consistent(),
            CredentialKind::OpenAiChatGpt => self.openai_credentials.is_consistent(),
        }
    }
    pub(super) fn apply_credential(
        &mut self,
        update: CredentialUpdate,
        generation: u64,
        marker: [u8; 16],
    ) -> Result<CredentialIdentityStatus, PersistenceError> {
        match update {
            CredentialUpdate::OpenCode(key) => self
                .credentials
                .apply(generation, marker, key)
                .map(Into::into),
            CredentialUpdate::OpenAi(tokens) => self.openai_credentials.apply(
                generation,
                marker,
                tokens,
                current_time_milliseconds()? / 1000,
            ),
        }
    }
}
