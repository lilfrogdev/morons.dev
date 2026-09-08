//! Reviewed native subscription transport; application/persistence own dispatch admission.
mod request;
#[cfg(test)]
mod tests;
mod transport;
mod turn;

use super::{ModelCapabilities, ModelDataUse, ModelRetention, ModelTrainingUse};
pub use request::{CodexRequest, CodexRequestLimits};
pub(super) use transport::{ENDPOINT as NATIVE_RESPONSES_ENDPOINT, credential_error};
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
/// Reviewed full-Responses entries, not a remote catalog or entitlement claim.
pub const MODELS: &[OpenAiCodexModel] = &[
    model("gpt-5.5", "GPT-5.5 (ChatGPT)"),
    model("gpt-6-astra", "GPT-6 Astra (ChatGPT)"),
    model("gpt-5.6-sol", "GPT-5.6 Sol (ChatGPT)"),
    model("gpt-5.6-luna", "GPT-5.6 Luna (ChatGPT)"),
    model("gpt-5.6-terra", "GPT-5.6 Terra (ChatGPT)"),
    model(
        "gpt-daybreak-blue-latest",
        "Daybreak Blue (ChatGPT; approval required)",
    ),
];

const fn model(id: &'static str, display_name: &'static str) -> OpenAiCodexModel {
    OpenAiCodexModel {
        id,
        display_name,
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
    }
}
