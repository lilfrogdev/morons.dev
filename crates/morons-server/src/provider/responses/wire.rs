use serde::Deserialize;
use serde_json::Value;

use crate::provider::ProviderMessagePhase;

#[derive(Deserialize)]
pub(super) struct EventEnvelope {
    #[serde(rename = "type")]
    pub(super) event_type: String,
    pub(super) sequence_number: u64,
}

#[derive(Deserialize)]
pub(super) struct LifecycleEvent {
    pub(super) response: LifecycleResponse,
}

#[derive(Deserialize)]
pub(super) struct LifecycleResponse {
    pub(super) id: String,
    pub(super) object: String,
    pub(super) status: String,
    pub(super) model: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TextDeltaEvent {
    #[serde(rename = "type")]
    pub(super) _event_type: String,
    pub(super) content_index: u32,
    pub(super) delta: String,
    pub(super) item_id: String,
    #[serde(default)]
    pub(super) logprobs: Vec<Value>,
    pub(super) output_index: u32,
    pub(super) sequence_number: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RefusalDeltaEvent {
    #[serde(rename = "type")]
    pub(super) _event_type: String,
    pub(super) content_index: u32,
    pub(super) delta: String,
    pub(super) item_id: String,
    pub(super) output_index: u32,
    pub(super) sequence_number: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FunctionArgumentsDeltaEvent {
    #[serde(rename = "type")]
    pub(super) _event_type: String,
    pub(super) delta: String,
    pub(super) item_id: String,
    pub(super) output_index: u32,
    #[serde(rename = "sequence_number")]
    pub(super) _sequence_number: u64,
}

#[derive(Deserialize)]
pub(super) struct CompletedEvent {
    pub(super) response: CompletedResponse,
}

#[derive(Deserialize)]
pub(super) struct CompletedResponse {
    pub(super) id: String,
    pub(super) object: String,
    pub(super) model: String,
    pub(super) status: String,
    pub(super) output: Vec<Value>,
    pub(super) usage: Option<WireUsage>,
}

#[derive(Deserialize)]
pub(super) struct WireOutputMessage {
    pub(super) id: String,
    pub(super) content: Vec<Value>,
    pub(super) role: String,
    pub(super) status: String,
    pub(super) phase: Option<ProviderMessagePhase>,
}

#[derive(Deserialize)]
pub(super) struct WireOutputText {
    pub(super) annotations: Vec<Value>,
    pub(super) text: String,
    pub(super) logprobs: Option<Vec<Value>>,
}

#[derive(Deserialize)]
pub(super) struct WireRefusal {
    pub(super) refusal: String,
}

#[derive(Deserialize)]
pub(super) struct WireReasoning {
    pub(super) id: String,
    pub(super) summary: Vec<WireReasoningSummary>,
    pub(super) encrypted_content: Option<String>,
    pub(super) content: Option<Vec<Value>>,
    pub(super) status: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct WireReasoningSummary {
    #[serde(rename = "type")]
    pub(super) summary_type: String,
    pub(super) text: String,
}

#[derive(Deserialize)]
pub(super) struct WireFunctionCall {
    pub(super) arguments: String,
    pub(super) call_id: String,
    pub(super) name: String,
    pub(super) id: Option<String>,
    pub(super) status: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct WireUsage {
    pub(super) input_tokens: u64,
    pub(super) input_tokens_details: WireInputTokenDetails,
    pub(super) output_tokens: u64,
    pub(super) output_tokens_details: WireOutputTokenDetails,
    pub(super) total_tokens: u64,
}

#[derive(Deserialize)]
pub(super) struct WireInputTokenDetails {
    #[serde(default)]
    pub(super) cached_tokens: u64,
    #[serde(default)]
    pub(super) cache_write_tokens: u64,
}

#[derive(Deserialize)]
pub(super) struct WireOutputTokenDetails {
    #[serde(default)]
    pub(super) reasoning_tokens: u64,
}
