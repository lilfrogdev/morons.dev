mod cancellation;
mod catalog;
mod error;
pub(crate) mod json;
mod models;
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
    MAXIMUM_CONTEXT_TOKENS, MAXIMUM_INPUT_TOKENS, MAXIMUM_OUTPUT_TOKENS, ModelCapabilities,
    ModelDataUse, ModelRetention, ModelTrainingUse, OpenCodeModel, OpenCodeService,
    RESPONSES_PROTOCOL_REVISION, find_open_code_model, open_code_models,
};
pub(crate) use opencode::OpenCodeProvider;
pub use outcome::{
    ProviderAssistantMessage, ProviderOutcome, ProviderOutputItem, ProviderReasoning,
    ProviderStreamEvent, ProviderToolCall, ProviderUsage,
};
pub use request::{
    OpenCodeResponseRequest, ProviderContentPart, ProviderInputItem, ProviderMessagePhase,
    ProviderMessageRole, ProviderTool,
};
