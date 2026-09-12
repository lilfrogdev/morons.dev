use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::debug_log::{DebugFinish, DebugStage};

use super::{
    ProviderAssistantMessage, ProviderError, ProviderMessagePhase, ProviderOutcome,
    ProviderOutputItem, ProviderStreamEvent, ProviderToolCall, ProviderUsage,
    json::parse_strict_value,
    request::{
        MAX_PROVIDER_CALL_ID_BYTES, MAX_TOOL_ARGUMENT_BYTES, MAX_TOOL_COUNT, validate_identifier,
        validate_tool_name,
    },
    sse::{SseDecoder, SseRecord},
};

const MAX_PROVIDER_IDENTIFIER_BYTES: usize = 128;
const MAX_DELTA_BYTES: usize = 64 * 1024;
const MAX_ACCUMULATED_TEXT_BYTES: usize = 1024 * 1024;
const MAX_USAGE_TOKENS: u64 = 10_000_000;
const MAX_EVENT_DEPTH: usize = 32;
const MAX_EVENT_NODES: usize = 100_000;
const MAX_EVENT_COLLECTION_ITEMS: usize = 10_000;
const MAX_EVENT_OBJECT_FIELDS: usize = 256;
const MAX_EVENT_KEY_BYTES: usize = 128;

#[derive(Default)]
struct ToolCallAccumulator {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ChatDiagnosticSnapshot {
    pub(super) stage: DebugStage,
    pub(super) finish: DebugFinish,
    pub(super) done: bool,
    pub(super) usage_seen: bool,
}

pub(super) struct ChatCompletionsDecoder {
    diagnostic: ChatDiagnosticSnapshot,
    sse: SseDecoder,
    expected_model: &'static str,
    maximum_input_tokens: u32,
    maximum_output_tokens: u32,
    response_id: Option<String>,
    role_seen: bool,
    text: String,
    refusal: Option<bool>,
    ignored_reasoning_bytes: usize,
    tool_calls: BTreeMap<u32, ToolCallAccumulator>,
    finish_reason: Option<String>,
    usage: Option<ProviderUsage>,
    provider_sequence: u64,
    done: bool,
    post_terminal_envelope_seen: bool,
}

impl ChatCompletionsDecoder {
    pub(super) fn new(
        expected_model: &'static str,
        maximum_input_tokens: u32,
        maximum_output_tokens: u32,
    ) -> Self {
        Self {
            diagnostic: ChatDiagnosticSnapshot {
                stage: DebugStage::SseFraming,
                finish: DebugFinish::Absent,
                done: false,
                usage_seen: false,
            },
            sse: SseDecoder::new(),
            expected_model,
            maximum_input_tokens,
            maximum_output_tokens,
            response_id: None,
            role_seen: false,
            text: String::new(),
            refusal: None,
            ignored_reasoning_bytes: 0,
            tool_calls: BTreeMap::new(),
            finish_reason: None,
            usage: None,
            provider_sequence: 0,
            done: false,
            post_terminal_envelope_seen: false,
        }
    }

    pub(super) const fn diagnostic_snapshot(&self) -> ChatDiagnosticSnapshot {
        self.diagnostic
    }

    pub(super) fn push(&mut self, chunk: &[u8]) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
        self.diagnostic.stage = DebugStage::SseFraming;
        let records = self.sse.push(chunk)?;
        let mut events = Vec::new();
        for record in records {
            events.extend(self.process_record(record)?);
        }
        Ok(events)
    }

    #[cfg(test)]
    pub(super) fn finish(self) -> Result<ProviderOutcome, ProviderError> {
        self.finish_diagnosed().0
    }

    pub(super) fn finish_diagnosed(
        self,
    ) -> (
        Result<ProviderOutcome, ProviderError>,
        ChatDiagnosticSnapshot,
    ) {
        let mut snapshot = self.diagnostic_snapshot();
        let result = self.finish_inner(&mut snapshot);
        (result, snapshot)
    }

    fn finish_inner(
        self,
        snapshot: &mut ChatDiagnosticSnapshot,
    ) -> Result<ProviderOutcome, ProviderError> {
        snapshot.stage = DebugStage::SseFraming;
        self.sse.finish()?;
        snapshot.stage = DebugStage::Termination;
        if !self.done {
            return Err(ProviderError::IncompleteResponse);
        }
        snapshot.stage = DebugStage::Identity;
        let response_id = self.response_id.ok_or(ProviderError::IncompleteResponse)?;
        snapshot.stage = DebugStage::Usage;
        let usage = self.usage.ok_or(ProviderError::IncompleteResponse)?;
        snapshot.stage = DebugStage::FinishReason;
        let finish_reason = self
            .finish_reason
            .as_deref()
            .ok_or(ProviderError::IncompleteResponse)?;
        match finish_reason {
            "stop" if self.tool_calls.is_empty() => {}
            "tool_calls" if !self.tool_calls.is_empty() => {}
            "length" => return Err(ProviderError::IncompleteResponse),
            "content_filter" | "sensitive" | "network_error" => {
                return Err(ProviderError::ProviderExecutionFailed);
            }
            "model_context_window_exceeded" => return Err(ProviderError::IncompleteResponse),
            _ => return Err(ProviderError::MalformedResponse),
        }
        snapshot.stage = DebugStage::Delta;
        if !self.role_seen || (self.text.is_empty() && self.tool_calls.is_empty()) {
            return Err(ProviderError::MalformedResponse);
        }
        let has_tool_calls = !self.tool_calls.is_empty();
        let mut output = Vec::new();
        if !self.text.is_empty() {
            output.push(ProviderOutputItem::AssistantMessage(
                ProviderAssistantMessage {
                    provider_item_id: response_id.clone(),
                    phase: Some(if has_tool_calls {
                        ProviderMessagePhase::Commentary
                    } else {
                        ProviderMessagePhase::FinalAnswer
                    }),
                    text: self.text,
                    refusal: self.refusal.unwrap_or(false),
                },
            ));
        }
        for (expected_index, (index, call)) in self.tool_calls.into_iter().enumerate() {
            snapshot.stage = DebugStage::ToolCall;
            if usize::try_from(index).ok() != Some(expected_index) {
                return Err(ProviderError::MalformedResponse);
            }
            let id = call.id.ok_or(ProviderError::IncompleteResponse)?;
            let name = call.name.ok_or(ProviderError::IncompleteResponse)?;
            validate_identifier(&id, MAX_PROVIDER_CALL_ID_BYTES)?;
            validate_tool_name(&name)?;
            snapshot.stage = DebugStage::ArgumentJson;
            if call.arguments.is_empty() || call.arguments.len() > MAX_TOOL_ARGUMENT_BYTES {
                return Err(ProviderError::MalformedResponse);
            }
            let arguments = parse_strict_value(call.arguments.as_bytes())
                .map_err(|_| ProviderError::MalformedResponse)?;
            if !arguments.is_object() {
                return Err(ProviderError::MalformedResponse);
            }
            output.push(ProviderOutputItem::ToolCall(ProviderToolCall {
                provider_item_id: None,
                provider_call_id: id,
                name,
                arguments: call.arguments,
                opaque_continuation: None,
            }));
        }
        snapshot.stage = DebugStage::Complete;
        Ok(ProviderOutcome {
            provider_response_id: response_id,
            output,
            usage,
        })
    }

    fn process_record(
        &mut self,
        record: SseRecord,
    ) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
        self.diagnostic.stage = DebugStage::Envelope;
        if record.event.is_some() {
            return Err(ProviderError::MalformedResponse);
        }
        if self.done {
            self.diagnostic.stage = DebugStage::Termination;
            if is_done_marker(&record.data) {
                return Ok(Vec::new());
            }
            if self.post_terminal_envelope_seen {
                return Err(ProviderError::MalformedResponse);
            }
            self.diagnostic.stage = DebugStage::Json;
            let value =
                parse_strict_value(&record.data).map_err(|_| ProviderError::MalformedResponse)?;
            self.diagnostic.usage_seen |= value.get("usage").is_some_and(|usage| !usage.is_null());
            self.diagnostic.stage = DebugStage::Envelope;
            let mut nodes = 0_usize;
            validate_event_value(&value, 0, &mut nodes)?;
            if let Ok(trailer) = serde_json::from_value::<ChatCostTrailer>(value.clone())
                && trailer.choices.is_empty()
                && trailer.cost.is_valid()
            {
                self.post_terminal_envelope_seen = true;
                return Ok(Vec::new());
            }
            let chunk: ChatChunk =
                serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
            self.validate_chunk_identity(&chunk)?;
            self.diagnostic.stage = DebugStage::Choices;
            return if chunk.choices.is_empty() && chunk.usage.is_none() {
                self.post_terminal_envelope_seen = true;
                Ok(Vec::new())
            } else {
                Err(ProviderError::MalformedResponse)
            };
        }
        if is_done_marker(&record.data) {
            self.diagnostic.stage = DebugStage::Termination;
            if self.finish_reason.is_none() || self.usage.is_none() {
                return Err(ProviderError::IncompleteResponse);
            }
            self.done = true;
            self.diagnostic.done = true;
            return Ok(Vec::new());
        }
        self.diagnostic.stage = DebugStage::Json;
        let value =
            parse_strict_value(&record.data).map_err(|_| ProviderError::MalformedResponse)?;
        self.diagnostic.usage_seen |= value.get("usage").is_some_and(|usage| !usage.is_null());
        self.diagnostic.stage = DebugStage::Envelope;
        let mut nodes = 0_usize;
        validate_event_value(&value, 0, &mut nodes)?;
        let chunk: ChatChunk =
            serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
        self.validate_chunk_identity(&chunk)?;
        self.diagnostic.stage = DebugStage::Choices;
        let terminal_usage_choice = self.finish_reason.as_deref().is_some_and(|finish_reason| {
            chunk.usage.is_some()
                && self.usage.is_none()
                && chunk.choices.len() == 1
                && chunk.choices[0].is_terminal_usage_noop(finish_reason)
        });
        if chunk.choices.is_empty() {
            if chunk.usage.is_none() || self.finish_reason.is_none() {
                return Err(ProviderError::MalformedResponse);
            }
        } else if !terminal_usage_choice
            && (chunk.choices.len() != 1 || self.finish_reason.is_some())
        {
            return Err(ProviderError::MalformedResponse);
        }

        let mut events = Vec::new();
        if !terminal_usage_choice && let Some(choice) = chunk.choices.into_iter().next() {
            if choice.index != 0 || choice.logprobs.is_some() {
                return Err(ProviderError::MalformedResponse);
            }
            events.extend(self.process_delta(choice.delta)?);
            if let Some(reason) = choice.finish_reason {
                self.diagnostic.stage = DebugStage::FinishReason;
                self.diagnostic.finish = match reason.as_str() {
                    "stop" => DebugFinish::Stop,
                    "tool_calls" => DebugFinish::ToolCalls,
                    "length" => DebugFinish::Length,
                    "content_filter" => DebugFinish::ContentFilter,
                    "sensitive" => DebugFinish::Sensitive,
                    "network_error" => DebugFinish::NetworkError,
                    "model_context_window_exceeded" => DebugFinish::ContextWindowExceeded,
                    _ => DebugFinish::Other,
                };
                if reason.is_empty() || reason.len() > MAX_PROVIDER_IDENTIFIER_BYTES {
                    return Err(ProviderError::MalformedResponse);
                }
                self.finish_reason = Some(reason);
            }
        }
        if let Some(usage) = chunk.usage {
            self.diagnostic.stage = DebugStage::Usage;
            let usage = self.validate_usage(usage)?;
            match self.usage {
                Some(existing) if !usage_refines(existing, usage) => {
                    return Err(ProviderError::MalformedResponse);
                }
                Some(_) | None => self.usage = Some(usage),
            }
        }
        Ok(events)
    }

    fn validate_chunk_identity(&mut self, chunk: &ChatChunk) -> Result<(), ProviderError> {
        self.diagnostic.stage = DebugStage::Identity;
        if chunk
            .object
            .as_deref()
            .is_some_and(|object| object != "chat.completion.chunk")
            || chunk.model != self.expected_model
            || chunk.created == 0
            || chunk.id.len() > MAX_PROVIDER_IDENTIFIER_BYTES
        {
            return Err(ProviderError::MalformedResponse);
        }
        validate_identifier(&chunk.id, MAX_PROVIDER_IDENTIFIER_BYTES)?;
        if chunk
            .request_id
            .as_ref()
            .is_some_and(|value| validate_identifier(value, MAX_PROVIDER_IDENTIFIER_BYTES).is_err())
            || chunk.system_fingerprint.as_ref().is_some_and(|value| {
                value.is_empty() || value.len() > MAX_PROVIDER_IDENTIFIER_BYTES
            })
            || chunk.service_tier.as_ref().is_some_and(|value| {
                value.is_empty() || value.len() > MAX_PROVIDER_IDENTIFIER_BYTES
            })
            || chunk
                .web_search
                .as_ref()
                .is_some_and(|results| !results.is_empty())
        {
            return Err(ProviderError::MalformedResponse);
        }
        match &self.response_id {
            Some(expected) if expected != &chunk.id => Err(ProviderError::MalformedResponse),
            Some(_) => Ok(()),
            None => {
                self.response_id = Some(chunk.id.clone());
                Ok(())
            }
        }
    }

    fn process_delta(
        &mut self,
        delta: ChatDelta,
    ) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
        self.diagnostic.stage = DebugStage::Delta;
        if let Some(role) = delta.role {
            if role != "assistant" {
                return Err(ProviderError::MalformedResponse);
            }
            self.role_seen = true;
        }
        if delta.function_call.is_some() {
            return Err(ProviderError::MalformedResponse);
        }
        for reasoning in [delta.reasoning, delta.reasoning_content]
            .into_iter()
            .flatten()
        {
            if reasoning.len() > MAX_DELTA_BYTES {
                return Err(ProviderError::ResponseLimitExceeded);
            }
            self.ignored_reasoning_bytes = self
                .ignored_reasoning_bytes
                .checked_add(reasoning.len())
                .filter(|bytes| *bytes <= MAX_ACCUMULATED_TEXT_BYTES)
                .ok_or(ProviderError::ResponseLimitExceeded)?;
        }
        if let Some(details) = delta.reasoning_details {
            let encoded =
                serde_json::to_vec(&details).map_err(|_| ProviderError::MalformedResponse)?;
            if encoded.len() > MAX_DELTA_BYTES {
                return Err(ProviderError::ResponseLimitExceeded);
            }
            self.ignored_reasoning_bytes = self
                .ignored_reasoning_bytes
                .checked_add(encoded.len())
                .filter(|bytes| *bytes <= MAX_ACCUMULATED_TEXT_BYTES)
                .ok_or(ProviderError::ResponseLimitExceeded)?;
        }

        let mut events = Vec::new();
        if let Some(content) = delta.content.filter(|content| !content.is_empty()) {
            events.push(self.record_text_delta(content, false)?);
        }
        if let Some(refusal) = delta.refusal.filter(|refusal| !refusal.is_empty()) {
            events.push(self.record_text_delta(refusal, true)?);
        }
        if let Some(calls) = delta.tool_calls {
            self.diagnostic.stage = DebugStage::ToolCall;
            if calls.len() > MAX_TOOL_COUNT {
                return Err(ProviderError::ResponseLimitExceeded);
            }
            for call in calls {
                self.record_tool_call_delta(call)?;
            }
        }
        Ok(events)
    }

    fn record_text_delta(
        &mut self,
        delta: String,
        refusal: bool,
    ) -> Result<ProviderStreamEvent, ProviderError> {
        if delta.len() > MAX_DELTA_BYTES {
            return Err(ProviderError::ResponseLimitExceeded);
        }
        if self.refusal.is_some_and(|existing| existing != refusal) {
            return Err(ProviderError::MalformedResponse);
        }
        self.refusal = Some(refusal);
        self.text
            .len()
            .checked_add(delta.len())
            .filter(|bytes| *bytes <= MAX_ACCUMULATED_TEXT_BYTES)
            .ok_or(ProviderError::ResponseLimitExceeded)?;
        self.text.push_str(&delta);
        let provider_sequence = self.provider_sequence;
        self.provider_sequence = self
            .provider_sequence
            .checked_add(1)
            .ok_or(ProviderError::ResponseLimitExceeded)?;
        Ok(ProviderStreamEvent::TextDelta {
            provider_sequence,
            output_index: 0,
            content_index: 0,
            delta,
            refusal,
        })
    }

    fn record_tool_call_delta(&mut self, call: ChatToolCallDelta) -> Result<(), ProviderError> {
        if usize::try_from(call.index).map_or(true, |index| index >= MAX_TOOL_COUNT)
            || call
                .call_type
                .as_deref()
                .is_some_and(|value| value != "function")
        {
            return Err(ProviderError::MalformedResponse);
        }
        let accumulator = self.tool_calls.entry(call.index).or_default();
        if let Some(id) = call.id {
            validate_identifier(&id, MAX_PROVIDER_CALL_ID_BYTES)?;
            if accumulator
                .id
                .as_ref()
                .is_some_and(|existing| existing != &id)
            {
                return Err(ProviderError::MalformedResponse);
            }
            accumulator.id = Some(id);
        }
        if let Some(function) = call.function {
            if let Some(name) = function.name {
                validate_tool_name(&name)?;
                if accumulator
                    .name
                    .as_ref()
                    .is_some_and(|existing| existing != &name)
                {
                    return Err(ProviderError::MalformedResponse);
                }
                accumulator.name = Some(name);
            }
            if let Some(arguments) = function.arguments {
                if arguments.len() > MAX_DELTA_BYTES {
                    return Err(ProviderError::ResponseLimitExceeded);
                }
                accumulator
                    .arguments
                    .len()
                    .checked_add(arguments.len())
                    .filter(|bytes| *bytes <= MAX_TOOL_ARGUMENT_BYTES)
                    .ok_or(ProviderError::ResponseLimitExceeded)?;
                accumulator.arguments.push_str(&arguments);
            }
        }
        Ok(())
    }

    fn validate_usage(&self, usage: ChatUsage) -> Result<ProviderUsage, ProviderError> {
        let detailed_cached = usage
            .prompt_tokens_details
            .as_ref()
            .and_then(|details| details.cached_tokens);
        let cache_write = usage
            .prompt_tokens_details
            .as_ref()
            .and_then(|details| details.cache_write_tokens)
            .unwrap_or(0);
        let cached = match (detailed_cached, usage.prompt_cache_hit_tokens) {
            (Some(detailed), Some(top_level)) if detailed != top_level => {
                return Err(ProviderError::MalformedResponse);
            }
            (Some(detailed), _) => detailed,
            (None, Some(top_level)) => top_level,
            (None, None) => 0,
        };
        let reasoning = usage
            .completion_tokens_details
            .as_ref()
            .and_then(|details| details.reasoning_tokens)
            .unwrap_or(0);
        let unsupported_usage = usage
            .prompt_tokens_details
            .as_ref()
            .and_then(|details| details.audio_tokens)
            .is_some_and(|tokens| tokens != 0)
            || usage
                .completion_tokens_details
                .as_ref()
                .is_some_and(|details| {
                    [
                        details.audio_tokens,
                        details.accepted_prediction_tokens,
                        details.rejected_prediction_tokens,
                    ]
                    .into_iter()
                    .flatten()
                    .any(|tokens| tokens != 0)
                });
        let prompt_zero = usage.prompt_tokens == 0;
        let completion_zero = usage.completion_tokens == 0;
        let input_limit = usage.prompt_tokens > u64::from(self.maximum_input_tokens)
            || usage.prompt_tokens > MAX_USAGE_TOKENS;
        let output_limit = usage.completion_tokens > u64::from(self.maximum_output_tokens)
            || usage.completion_tokens > MAX_USAGE_TOKENS;
        let total_mismatch =
            usage.prompt_tokens.checked_add(usage.completion_tokens) != Some(usage.total_tokens);
        let invalid_cache = cached
            .checked_add(cache_write)
            .is_none_or(|cache_tokens| cache_tokens > usage.prompt_tokens);
        let miss_mismatch = usage
            .prompt_cache_miss_tokens
            .is_some_and(|miss| usage.prompt_tokens.checked_sub(cached) != Some(miss));
        let invalid_reasoning = reasoning > usage.completion_tokens;
        if unsupported_usage
            || prompt_zero
            || completion_zero
            || input_limit
            || output_limit
            || total_mismatch
            || invalid_cache
            || miss_mismatch
            || invalid_reasoning
        {
            return Err(ProviderError::MalformedResponse);
        }
        Ok(ProviderUsage {
            input_tokens: usage.prompt_tokens,
            cached_input_tokens: cached,
            cache_write_input_tokens: cache_write,
            output_tokens: usage.completion_tokens,
            reasoning_output_tokens: reasoning,
            total_tokens: usage.total_tokens,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatChunk {
    id: String,
    object: Option<String>,
    request_id: Option<String>,
    created: u64,
    model: String,
    choices: Vec<ChatChoice>,
    usage: Option<ChatUsage>,
    system_fingerprint: Option<String>,
    service_tier: Option<String>,
    web_search: Option<Vec<Value>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatCostTrailer {
    choices: Vec<Value>,
    cost: ChatCost,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ChatCost {
    Number(serde_json::Number),
    String(String),
}

impl ChatCost {
    fn is_valid(&self) -> bool {
        let value = match self {
            Self::Number(value) => value.as_f64(),
            Self::String(value) if !value.is_empty() && value.len() <= 64 => value.parse().ok(),
            Self::String(_) => None,
        };
        value.is_some_and(|value: f64| value.is_finite() && (0.0..=1_000_000.0).contains(&value))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatChoice {
    index: u32,
    delta: ChatDelta,
    finish_reason: Option<String>,
    logprobs: Option<Value>,
}

impl ChatChoice {
    fn is_terminal_usage_noop(&self, expected_finish_reason: &str) -> bool {
        self.index == 0
            && self.finish_reason.as_deref() == Some(expected_finish_reason)
            && self.logprobs.is_none()
            && self.delta.is_terminal_noop()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatDelta {
    role: Option<String>,
    content: Option<String>,
    refusal: Option<String>,
    tool_calls: Option<Vec<ChatToolCallDelta>>,
    function_call: Option<Value>,
    reasoning: Option<String>,
    reasoning_content: Option<String>,
    reasoning_details: Option<Vec<Value>>,
}

impl ChatDelta {
    fn is_terminal_noop(&self) -> bool {
        self.role.as_deref().is_none_or(|role| role == "assistant")
            && self.content.as_deref().is_none_or(str::is_empty)
            && self.refusal.as_deref().is_none_or(str::is_empty)
            && self.tool_calls.as_ref().is_none_or(Vec::is_empty)
            && self.function_call.is_none()
            && self.reasoning.as_deref().is_none_or(str::is_empty)
            && self.reasoning_content.as_deref().is_none_or(str::is_empty)
            && self.reasoning_details.as_ref().is_none_or(Vec::is_empty)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatToolCallDelta {
    index: u32,
    id: Option<String>,
    #[serde(rename = "type")]
    call_type: Option<String>,
    function: Option<ChatFunctionDelta>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    prompt_tokens_details: Option<PromptTokenDetails>,
    completion_tokens_details: Option<CompletionTokenDetails>,
    prompt_cache_hit_tokens: Option<u64>,
    prompt_cache_miss_tokens: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptTokenDetails {
    cached_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
    audio_tokens: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompletionTokenDetails {
    reasoning_tokens: Option<u64>,
    audio_tokens: Option<u64>,
    accepted_prediction_tokens: Option<u64>,
    rejected_prediction_tokens: Option<u64>,
}

const fn usage_refines(previous: ProviderUsage, next: ProviderUsage) -> bool {
    previous.input_tokens == next.input_tokens
        && previous.output_tokens <= next.output_tokens
        && previous.total_tokens <= next.total_tokens
        && previous.cached_input_tokens <= next.cached_input_tokens
        && previous.cache_write_input_tokens <= next.cache_write_input_tokens
        && previous.reasoning_output_tokens <= next.reasoning_output_tokens
}

fn is_done_marker(data: &[u8]) -> bool {
    let start = data
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(data.len());
    let end = data
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    data.get(start..end) == Some(&b"[DONE]"[..])
}

fn validate_event_value(
    value: &Value,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), ProviderError> {
    if depth > MAX_EVENT_DEPTH {
        return Err(ProviderError::ResponseLimitExceeded);
    }
    *nodes = nodes
        .checked_add(1)
        .filter(|nodes| *nodes <= MAX_EVENT_NODES)
        .ok_or(ProviderError::ResponseLimitExceeded)?;
    match value {
        Value::Array(values) => {
            if values.len() > MAX_EVENT_COLLECTION_ITEMS {
                return Err(ProviderError::ResponseLimitExceeded);
            }
            for value in values {
                validate_event_value(value, depth + 1, nodes)?;
            }
        }
        Value::Object(values) => {
            if values.len() > MAX_EVENT_OBJECT_FIELDS {
                return Err(ProviderError::ResponseLimitExceeded);
            }
            for (key, value) in values {
                if key.len() > MAX_EVENT_KEY_BYTES {
                    return Err(ProviderError::ResponseLimitExceeded);
                }
                validate_event_value(value, depth + 1, nodes)?;
            }
        }
        Value::String(value) if value.len() > MAX_ACCUMULATED_TEXT_BYTES => {
            return Err(ProviderError::ResponseLimitExceeded);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

#[cfg(test)]
mod diagnostic_tests;

#[cfg(test)]
mod tests;
