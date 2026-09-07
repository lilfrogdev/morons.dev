//! Reviewed native subscription transport; application/persistence own dispatch admission.
mod request;
#[cfg(test)]
mod tests;
mod transport;
mod turn;

use super::{ModelCapabilities, ModelDataUse, ModelRetention, ModelTrainingUse};
pub use request::{CodexRequest, CodexRequestLimits};
pub use transport::{OpenAiCodexProvider, PreparedCodexDispatch};
pub use turn::CodexTurn;

pub const CODEX_RESPONSES_PROTOCOL_REVISION: u16 = 5;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenAiCodexModel {
    pub id: &'static str,
    pub display_name: &'static str,
    pub protocol_revision: u16,
    pub maximum_input_tokens: u32,
    /// Local acceptance only; the native backend does not take max_output_tokens.
    pub maximum_output_tokens: u32,
    pub capabilities: ModelCapabilities,
    /// Unverifiable account/workspace policy is deliberately not favorable metadata.
    pub data_use: ModelDataUse,
}
pub const MODELS: &[OpenAiCodexModel] = &[OpenAiCodexModel {
    id: "gpt-5.5",
    display_name: "GPT-5.5 (ChatGPT)",
    protocol_revision: CODEX_RESPONSES_PROTOCOL_REVISION,
    maximum_input_tokens: super::MAXIMUM_INPUT_TOKENS,
    maximum_output_tokens: super::MAXIMUM_OUTPUT_TOKENS,
    capabilities: ModelCapabilities {
        text_input: true,
        image_input: true,
        text_output: true,
        reasoning: true,
        reasoning_continuation: true,
        tool_calls: true,
    },
    data_use: ModelDataUse {
        training: ModelTrainingUse::NotDocumented,
        retention: ModelRetention::NotDocumented,
    },
}];
