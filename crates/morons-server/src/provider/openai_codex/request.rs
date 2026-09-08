use super::{CodexTurn, turn::Identity};
use crate::provider::{
    DataUseRestrictions, PreparedProviderTools, ProviderError, ProviderInputItem, ProviderProtocol,
    ProviderTool,
    request::{
        MAX_AGGREGATE_INPUT_BYTES, MAX_PROVIDER_REQUEST_BYTES, responses_input, responses_tools,
        validate_input,
    },
};
use bytes::Bytes;
use serde::Serialize;
use std::{fmt, sync::Arc};

/// Local validation only; maximum_output_tokens is not sent or enforced as a spending cap.
#[derive(Clone, Copy, Debug)]
pub struct CodexRequestLimits {
    pub estimated_input_tokens: u32,
    pub maximum_output_tokens: u32,
}
pub struct CodexRequest {
    pub(super) identity: Arc<Identity>,
    pub(super) sequence: u64,
    pub(super) body: Bytes,
    pub(super) limits: CodexRequestLimits,
}
impl fmt::Debug for CodexRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodexRequest")
            .field("model", &self.identity.model.id)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}
impl CodexRequest {
    pub fn new(
        turn: &CodexTurn,
        instructions: &str,
        input: Vec<ProviderInputItem>,
        tools: Vec<ProviderTool>,
        limits: CodexRequestLimits,
        policy: DataUseRestrictions,
    ) -> Result<Self, ProviderError> {
        Self::with_prepared_tools(
            turn,
            instructions,
            &input,
            &PreparedProviderTools::new(tools)?,
            limits,
            policy,
        )
    }
    pub(crate) fn with_prepared_tools(
        turn: &CodexTurn,
        instructions: &str,
        input: &[ProviderInputItem],
        tools: &PreparedProviderTools,
        limits: CodexRequestLimits,
        policy: DataUseRestrictions,
    ) -> Result<Self, ProviderError> {
        let model = turn.model();
        if !policy.permits(model.data_use) {
            return Err(ProviderError::DataUseRestricted);
        }
        if !turn.usable
            || instructions.is_empty()
            || instructions.len() > 64 * 1024
            || limits.estimated_input_tokens == 0
            || limits.estimated_input_tokens > model.maximum_input_tokens
            || limits.maximum_output_tokens == 0
            || limits.maximum_output_tokens > model.maximum_output_tokens
        {
            return Err(ProviderError::InvalidRequest);
        }
        let input_bytes = validate_input(input, ProviderProtocol::Responses, model.capabilities)?;
        for item in input {
            if let ProviderInputItem::Reasoning {
                id,
                summaries,
                encrypted_content,
            } = item
                && !turn.reasoning.contains(&super::turn::reasoning_fingerprint(
                    id,
                    summaries,
                    encrypted_content.as_deref(),
                ))
            {
                return Err(ProviderError::InvalidRequest);
            }
        }
        if input_bytes
            .checked_add(instructions.len())
            .is_none_or(|n| n > MAX_AGGREGATE_INPUT_BYTES)
        {
            return Err(ProviderError::InvalidRequest);
        }
        let body = serde_json::to_vec(&Body {
            model: model.id,
            instructions,
            input: responses_input(input),
            tools: responses_tools(tools),
            tool_choice: "auto",
            parallel_tool_calls: false,
            store: false,
            stream: true,
            include: ["reasoning.encrypted_content"],
            reasoning: Reasoning { effort: "medium" },
            text: Text { verbosity: "low" },
            prompt_cache_key: &turn.identity.session,
        })
        .map_err(|_| ProviderError::InvalidRequest)?;
        if body.len() > MAX_PROVIDER_REQUEST_BYTES {
            return Err(ProviderError::InvalidRequest);
        }
        Ok(Self {
            identity: turn.identity.clone(),
            sequence: turn.sequence,
            body: Bytes::from(body),
            limits,
        })
    }
    pub(super) fn validate(
        &self,
        turn: &CodexTurn,
        policy: DataUseRestrictions,
    ) -> Result<(), ProviderError> {
        if !policy.permits(self.identity.model.data_use) {
            return Err(ProviderError::DataUseRestricted);
        }
        if !turn.usable
            || !Arc::ptr_eq(&self.identity, &turn.identity)
            || self.sequence != turn.sequence
        {
            return Err(ProviderError::InvalidRequest);
        }
        Ok(())
    }
}
#[derive(Serialize)]
struct Body<'a, I: Serialize, T: Serialize> {
    model: &'a str,
    instructions: &'a str,
    input: I,
    tools: T,
    tool_choice: &'static str,
    parallel_tool_calls: bool,
    reasoning: Reasoning,
    text: Text,
    store: bool,
    stream: bool,
    include: [&'static str; 1],
    prompt_cache_key: &'a str,
}
#[derive(Serialize)]
struct Reasoning {
    effort: &'static str,
}
#[derive(Serialize)]
struct Text {
    verbosity: &'static str,
}
