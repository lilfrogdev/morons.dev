use super::{
    HostedWebResult, SubagentUsage, ToolErrorKind, ToolInput, ToolOutput, ToolResult, WebCitation,
    WebReceipt,
};
use crate::{
    persistence::{PersistenceError, SessionStore, WebBinding},
    provider::{
        ProviderCancellation, ProviderError, openai_auth::OpenAiCredentialProvider,
        openai_web::SearchProvider,
    },
};
use sha2::{Digest as _, Sha256};
use std::sync::Arc;

pub(crate) struct WebSearchToolExecutor {
    sessions: Arc<SessionStore>,
    provider: SearchProvider,
}
impl WebSearchToolExecutor {
    pub(crate) fn new(sessions: Arc<SessionStore>) -> Self {
        let credentials = Arc::new(OpenAiCredentialProvider::new(sessions.clone()));
        Self {
            sessions,
            provider: SearchProvider::new(credentials),
        }
    }
    #[cfg(test)]
    pub(crate) fn for_test(sessions: Arc<SessionStore>, endpoint: String) -> Self {
        let credentials = Arc::new(OpenAiCredentialProvider::new(sessions.clone()));
        Self {
            sessions,
            provider: SearchProvider::for_test(
                credentials,
                endpoint.parse().expect("fixture route"),
            ),
        }
    }
    pub(crate) async fn execute(
        &self,
        input: &ToolInput,
        binding: &WebBinding,
        child: u16,
        ordinal: u64,
        cancellation: &ProviderCancellation,
    ) -> Result<ToolResult, PersistenceError> {
        let ToolInput::WebSearch { query } = input else {
            return Ok(ToolResult::error(ToolErrorKind::InvalidResponse));
        };
        let digest: [u8; 32] = Sha256::digest(query.as_bytes()).into();
        let scope_valid = if child == 0 {
            ordinal == 0 && binding.query_digest == Some(digest) && binding.children == 0
        } else {
            binding.query_digest.is_none() && child <= binding.children && ordinal > 0
        };
        if !scope_valid {
            return Err(PersistenceError::InvalidState {
                reason: "web search invocation does not match its owner",
            });
        }
        let mut cancellation = cancellation.clone();
        if cancellation.is_cancelled() {
            return Ok(ToolResult::error(ToolErrorKind::Cancelled));
        }
        let policy = match self.sessions.admit_web_binding(binding).await {
            Ok(p) => p.restrictions,
            Err(error) => return admission_error(error),
        };
        let operation = invocation_id(binding.operation_id, child, ordinal);
        let mut attempt =
            match self
                .provider
                .new_attempt(operation, binding.generation, query, policy)
            {
                Ok(a) => a,
                Err(e) => return preflight_error(e),
            };
        let dispatch = match self
            .provider
            .prepare(&mut attempt, policy, &mut cancellation)
            .await
        {
            Ok(d) => d,
            Err(e) => return preflight_error(e),
        };
        // This worker admission, after credential preparation, is the policy linearization point.
        let policy = match self.sessions.admit_web_binding(binding).await {
            Ok(p) => p.restrictions,
            Err(error) => return admission_error(error),
        };
        let result = match dispatch.execute(policy, &mut cancellation).await {
            Ok(result) => result,
            // Deliberately conservative: no complete outcome means service effects/usage may exist.
            Err(error) => {
                return Ok(ToolResult::error(ToolErrorKind::WebSearchUncertain(
                    attempt.failure(error),
                )));
            }
        };
        let usage = result.usage;
        Ok(ToolResult::Ok {
            output: ToolOutput::OpenAiWeb {
                result: HostedWebResult {
                    query: query.clone(),
                    answer: result.answer,
                    citations: result
                        .citations
                        .into_iter()
                        .map(|c| WebCitation {
                            title: c.title,
                            url: c.url,
                        })
                        .collect(),
                    receipt: WebReceipt {
                        model_id: crate::provider::openai_web::MODEL.to_owned(),
                        contract_revision: crate::provider::openai_web::CONTRACT_REVISION,
                        search_calls: result.search_calls,
                        open_page_calls: result.open_page_calls,
                        find_in_page_calls: result.find_in_page_calls,
                        usage: SubagentUsage {
                            input_tokens: usage.input_tokens,
                            cached_input_tokens: usage.cached_input_tokens,
                            cache_write_input_tokens: usage.cache_write_input_tokens,
                            output_tokens: usage.output_tokens,
                            reasoning_output_tokens: usage.reasoning_output_tokens,
                            total_tokens: usage.total_tokens,
                        },
                    },
                },
            },
        })
    }
}
fn invocation_id(owner: [u8; 16], child: u16, ordinal: u64) -> [u8; 16] {
    let bytes = Sha256::new()
        .chain_update(b"morons.dev/owned-web-invocation/v2\0")
        .chain_update(owner)
        .chain_update(child.to_be_bytes())
        .chain_update(ordinal.to_be_bytes())
        .finalize();
    let mut operation = [0; 16];
    operation.copy_from_slice(&bytes[..16]);
    operation
}

fn admission_error(error: PersistenceError) -> Result<ToolResult, PersistenceError> {
    match error {
        PersistenceError::OpenAiCredentialNotConfigured => {
            Ok(ToolResult::error(ToolErrorKind::CredentialNotConfigured))
        }
        PersistenceError::DataUseRestricted => {
            Ok(ToolResult::error(ToolErrorKind::DataUseRestricted))
        }
        PersistenceError::InvalidInput {
            reason: "web search owner is no longer active",
        } => Ok(ToolResult::error(ToolErrorKind::Cancelled)),
        error => Err(error),
    }
}
fn preflight_error(error: ProviderError) -> Result<ToolResult, PersistenceError> {
    let kind = match error {
        ProviderError::CredentialStoreUnavailable => {
            return Err(PersistenceError::InvalidState {
                reason: "web credential storage is unavailable",
            });
        }
        ProviderError::Cancelled => ToolErrorKind::Cancelled,
        ProviderError::DataUseRestricted => ToolErrorKind::DataUseRestricted,
        _ => ToolErrorKind::WebSearchUnavailable,
    };
    Ok(ToolResult::error(kind))
}

#[cfg(test)]
mod tests {
    use super::invocation_id;

    #[test]
    fn invocation_identity_preserves_wide_ordinals_and_owner_scope() {
        let owner = [1; 16];
        let ordinal = u64::from(u16::MAX) + 1;
        let id = invocation_id(owner, 1, ordinal);
        assert_eq!(id, invocation_id(owner, 1, ordinal));
        for other in [
            invocation_id(owner, 1, 0),
            invocation_id(owner, 1, ordinal - 1),
            invocation_id(owner, 1, ordinal + 1),
            invocation_id(owner, 2, ordinal),
            invocation_id([2; 16], 1, ordinal),
            invocation_id(owner, 1, u64::MAX),
        ] {
            assert_ne!(id, other);
        }
    }
}
