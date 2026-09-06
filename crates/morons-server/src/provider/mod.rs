mod anthropic_messages;
mod cancellation;
mod catalog;
mod chat_completions;
mod error;
mod gemini;
mod http_client;
pub(crate) mod json;
mod models;
pub mod openai_auth;
mod opencode;
mod outcome;
mod request;
mod responses;
mod sse;

pub(crate) use crate::persistence::OpenCodeCredentialLease;
pub use cancellation::{ProviderCancellation, ProviderCancellationHandle, provider_cancellation};
pub use catalog::OpenCodeModelAvailability;
pub use error::ProviderError;
pub use models::{
    ANTHROPIC_MESSAGES_PROTOCOL_REVISION, CHAT_COMPLETIONS_PROTOCOL_REVISION,
    GEMINI_PROTOCOL_REVISION, MAXIMUM_CONTEXT_TOKENS, MAXIMUM_INPUT_TOKENS, MAXIMUM_OUTPUT_TOKENS,
    ModelCapabilities, ModelDataUse, ModelRetention, ModelTrainingUse, OpenCodeModel,
    OpenCodeService, ProviderProtocol, RESPONSES_PROTOCOL_REVISION, find_open_code_model,
    open_code_models,
};
pub(crate) use opencode::OpenCodeProvider;
pub use outcome::{
    ProviderAssistantMessage, ProviderOutcome, ProviderOutputItem, ProviderReasoning,
    ProviderStreamEvent, ProviderToolCall, ProviderUsage,
};
pub(crate) use request::PreparedProviderTools;
pub use request::{
    OpenCodeResponseRequest, ProviderContentPart, ProviderInputItem, ProviderMessagePhase,
    ProviderMessageRole, ProviderTool,
};
