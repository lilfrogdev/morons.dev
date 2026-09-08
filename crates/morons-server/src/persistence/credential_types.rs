use super::OpenCodeCredentialStatus;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum CredentialKind {
    OpenCode = 1,
    OpenAiChatGpt = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CredentialIdentityStatus {
    pub configured: bool,
    pub generation: u64,
}

impl From<OpenCodeCredentialStatus> for CredentialIdentityStatus {
    fn from(value: OpenCodeCredentialStatus) -> Self {
        Self {
            configured: value.configured,
            generation: value.generation,
        }
    }
}
impl From<CredentialIdentityStatus> for OpenCodeCredentialStatus {
    fn from(value: CredentialIdentityStatus) -> Self {
        Self {
            configured: value.configured,
            generation: value.generation,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenAiCredentialState {
    Unconfigured,
    Configured,
    ReauthenticationRequired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenAiCredentialStatus {
    pub state: OpenAiCredentialState,
    pub generation: u64,
}
