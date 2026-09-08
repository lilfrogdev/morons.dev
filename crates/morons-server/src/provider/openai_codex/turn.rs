use super::{MODELS, OpenAiCodexModel};
use crate::provider::{ProviderError, response_diagnostic::ResponseStage};
use http::HeaderValue;
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeSet, fmt, sync::Arc};

pub(super) struct Identity {
    pub provider: Arc<()>,
    pub model: &'static OpenAiCodexModel,
    pub generation: u64,
    pub session: String,
    pub run: [u8; 16],
}
pub struct CodexTurn {
    pub(super) identity: Arc<Identity>,
    pub(super) sequence: u64,
    pub(super) usable: bool,
    pub(super) routing: Option<HeaderValue>,
    pub(super) reasoning: BTreeSet<[u8; 32]>,
    pub(super) failure: Option<ResponseStage>,
}
impl fmt::Debug for CodexTurn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodexTurn")
            .field("model", &self.identity.model.id)
            .field("usable", &self.usable)
            .finish_non_exhaustive()
    }
}
impl CodexTurn {
    pub(super) fn new(
        provider: Arc<()>,
        conversation: [u8; 16],
        run: [u8; 16],
        generation: u64,
        model_id: &str,
    ) -> Result<Self, ProviderError> {
        let model = MODELS
            .iter()
            .find(|m| m.id == model_id)
            .ok_or(ProviderError::UnsupportedModel)?;
        if conversation == [0; 16]
            || run == [0; 16]
            || generation == 0
            || generation > i64::MAX as u64
        {
            return Err(ProviderError::InvalidRequest);
        }
        Ok(Self {
            identity: Arc::new(Identity {
                provider,
                model,
                generation,
                run,
                session: identifier(
                    b"morons.dev/codex-session/v1\0",
                    &[&conversation, &generation.to_be_bytes()],
                ),
            }),
            sequence: 0,
            usable: true,
            routing: None,
            reasoning: BTreeSet::new(),
            failure: None,
        })
    }
    pub(crate) fn response_failure(&self) -> Option<ResponseStage> {
        self.failure
    }
    pub(super) fn record_failure(
        &mut self,
        error: ProviderError,
        stage: ResponseStage,
    ) -> ProviderError {
        if matches!(
            error,
            ProviderError::MalformedResponse
                | ProviderError::ResponseLimitExceeded
                | ProviderError::IncompleteResponse
                | ProviderError::UnexpectedContentType
                | ProviderError::RedirectDenied
        ) {
            self.failure = Some(stage);
        }
        error
    }
    pub fn model(&self) -> &'static OpenAiCodexModel {
        self.identity.model
    }
    pub(super) fn request_id(&self) -> String {
        identifier(
            b"morons.dev/codex-request/v1\0",
            &[
                self.identity.session.as_bytes(),
                &self.identity.run,
                &self.sequence.to_be_bytes(),
            ],
        )
    }
}
pub(super) fn reasoning_fingerprint(
    id: &str,
    summaries: &[String],
    encrypted: Option<&str>,
) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"morons.dev/codex-reasoning/v1\0");
    hash.update((summaries.len() as u64).to_be_bytes());
    for part in std::iter::once(id).chain(summaries.iter().map(String::as_str)) {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    hash.update([u8::from(encrypted.is_some())]);
    if let Some(encrypted) = encrypted {
        hash.update((encrypted.len() as u64).to_be_bytes());
        hash.update(encrypted.as_bytes());
    }
    hash.finalize().into()
}
fn identifier(domain: &[u8], parts: &[&[u8]]) -> String {
    let mut hash = Sha256::new();
    hash.update(domain);
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part);
    }
    let hash = hash.finalize();
    let hex = hash[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}
