use std::fmt;

use super::ProviderMessagePhase;

#[derive(Clone, PartialEq, Eq)]
pub struct ProviderAssistantMessage {
    pub provider_item_id: String,
    pub phase: Option<ProviderMessagePhase>,
    pub text: String,
    pub refusal: bool,
}

impl fmt::Debug for ProviderAssistantMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderAssistantMessage")
            .field("provider_item_id", &self.provider_item_id)
            .field("phase", &self.phase)
            .field("text_bytes", &self.text.len())
            .field("refusal", &self.refusal)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ProviderToolCall {
    pub provider_item_id: Option<String>,
    pub provider_call_id: String,
    pub name: String,
    pub arguments: String,
    pub opaque_continuation: Option<String>,
}

impl fmt::Debug for ProviderToolCall {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderToolCall")
            .field("provider_item_id", &self.provider_item_id)
            .field("provider_call_id", &self.provider_call_id)
            .field("name", &self.name)
            .field("argument_bytes", &self.arguments.len())
            .field(
                "opaque_continuation",
                &self.opaque_continuation.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ProviderReasoning {
    pub provider_item_id: String,
    pub summaries: Vec<String>,
    pub encrypted_content: Option<String>,
}

impl fmt::Debug for ProviderReasoning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderReasoning")
            .field("provider_item_id", &self.provider_item_id)
            .field("summary_count", &self.summaries.len())
            .field(
                "encrypted_content",
                &self.encrypted_content.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderOutputItem {
    AssistantMessage(ProviderAssistantMessage),
    Reasoning(ProviderReasoning),
    ToolCall(ProviderToolCall),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderUsage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_write_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ProviderOutcome {
    pub provider_response_id: String,
    pub output: Vec<ProviderOutputItem>,
    pub usage: ProviderUsage,
}

impl fmt::Debug for ProviderOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderOutcome")
            .field("provider_response_id", &self.provider_response_id)
            .field("output", &self.output)
            .field("usage", &self.usage)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum ProviderStreamEvent {
    TextDelta {
        provider_sequence: u64,
        output_index: u32,
        content_index: u32,
        delta: String,
        refusal: bool,
    },
}

impl fmt::Debug for ProviderStreamEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TextDelta {
                provider_sequence,
                output_index,
                content_index,
                delta,
                refusal,
            } => formatter
                .debug_struct("TextDelta")
                .field("provider_sequence", provider_sequence)
                .field("output_index", output_index)
                .field("content_index", content_index)
                .field("delta_bytes", &delta.len())
                .field("refusal", refusal)
                .finish(),
        }
    }
}
