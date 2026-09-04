use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;
#[cfg(debug_assertions)]
use sha2::{Digest as _, Sha256};

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

pub(super) struct ChatCompletionsDecoder {
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
    #[cfg(debug_assertions)]
    diagnostic_stage: &'static str,
}

impl ChatCompletionsDecoder {
    pub(super) fn new(
        expected_model: &'static str,
        maximum_input_tokens: u32,
        maximum_output_tokens: u32,
    ) -> Self {
        Self {
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
            #[cfg(debug_assertions)]
            diagnostic_stage: "awaiting an SSE record",
        }
    }

    #[cfg(debug_assertions)]
    pub(super) const fn diagnostic_stage(&self) -> &'static str {
        self.diagnostic_stage
    }

    pub(super) fn push(&mut self, chunk: &[u8]) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
        #[cfg(debug_assertions)]
        {
            self.diagnostic_stage = "decoding SSE framing";
        }
        let records = self.sse.push(chunk)?;
        let mut events = Vec::new();
        for record in records {
            events.extend(self.process_record(record)?);
        }
        Ok(events)
    }

    pub(super) fn finish(self) -> Result<ProviderOutcome, ProviderError> {
        self.sse.finish()?;
        if !self.done {
            return Err(ProviderError::IncompleteResponse);
        }
        let response_id = self.response_id.ok_or(ProviderError::IncompleteResponse)?;
        let usage = self.usage.ok_or(ProviderError::IncompleteResponse)?;
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
            if usize::try_from(index).ok() != Some(expected_index) {
                return Err(ProviderError::MalformedResponse);
            }
            let id = call.id.ok_or(ProviderError::IncompleteResponse)?;
            let name = call.name.ok_or(ProviderError::IncompleteResponse)?;
            validate_identifier(&id, MAX_PROVIDER_CALL_ID_BYTES)?;
            validate_tool_name(&name)?;
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
            }));
        }
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
        #[cfg(debug_assertions)]
        {
            self.diagnostic_stage = "validating an SSE record";
        }
        if record.event.is_some() {
            #[cfg(debug_assertions)]
            {
                self.diagnostic_stage = "validating the SSE event name";
            }
            return Err(ProviderError::MalformedResponse);
        }
        if self.done {
            #[cfg(debug_assertions)]
            {
                self.diagnostic_stage = "validating a post-terminal no-op";
            }
            if is_done_marker(&record.data) {
                return Ok(Vec::new());
            }
            if self.post_terminal_envelope_seen {
                return Err(ProviderError::MalformedResponse);
            }
            let value =
                parse_strict_value(&record.data).map_err(|_| ProviderError::MalformedResponse)?;
            let mut nodes = 0_usize;
            validate_event_value(&value, 0, &mut nodes)?;
            #[cfg(debug_assertions)]
            {
                self.diagnostic_stage = "decoding a post-terminal chunk";
                diagnose_unknown_chunk_fields(&value);
            }
            if let Ok(trailer) = serde_json::from_value::<ChatCostTrailer>(value.clone())
                && trailer.choices.is_empty()
                && trailer.cost.is_valid()
            {
                self.post_terminal_envelope_seen = true;
                return Ok(Vec::new());
            }
            let chunk: ChatChunk =
                serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
            #[cfg(debug_assertions)]
            {
                self.diagnostic_stage = "validating post-terminal chunk identity";
            }
            self.validate_chunk_identity(&chunk)?;
            #[cfg(debug_assertions)]
            {
                self.diagnostic_stage = "validating the post-terminal empty shape";
            }
            return if chunk.choices.is_empty() && chunk.usage.is_none() {
                self.post_terminal_envelope_seen = true;
                Ok(Vec::new())
            } else {
                Err(ProviderError::MalformedResponse)
            };
        }
        if is_done_marker(&record.data) {
            #[cfg(debug_assertions)]
            {
                self.diagnostic_stage = "validating the done marker";
            }
            if self.finish_reason.is_none() || self.usage.is_none() {
                return Err(ProviderError::IncompleteResponse);
            }
            self.done = true;
            return Ok(Vec::new());
        }
        #[cfg(debug_assertions)]
        {
            self.diagnostic_stage = "decoding strict chunk JSON";
        }
        let value =
            parse_strict_value(&record.data).map_err(|_| ProviderError::MalformedResponse)?;
        let mut nodes = 0_usize;
        validate_event_value(&value, 0, &mut nodes)?;
        #[cfg(debug_assertions)]
        {
            self.diagnostic_stage = "decoding the chunk structure";
            diagnose_unknown_chunk_fields(&value);
        }
        let chunk: ChatChunk =
            serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
        #[cfg(debug_assertions)]
        {
            self.diagnostic_stage = "validating chunk identity";
        }
        self.validate_chunk_identity(&chunk)?;
        #[cfg(debug_assertions)]
        {
            self.diagnostic_stage = "validating chunk choices";
        }
        if chunk.choices.is_empty() {
            if chunk.usage.is_none() || self.finish_reason.is_none() {
                return Err(ProviderError::MalformedResponse);
            }
        } else if chunk.choices.len() != 1 || self.finish_reason.is_some() {
            return Err(ProviderError::MalformedResponse);
        }

        let mut events = Vec::new();
        if let Some(choice) = chunk.choices.into_iter().next() {
            #[cfg(debug_assertions)]
            {
                self.diagnostic_stage = "validating a choice delta";
            }
            if choice.index != 0 || choice.logprobs.is_some() {
                return Err(ProviderError::MalformedResponse);
            }
            events.extend(self.process_delta(choice.delta)?);
            if let Some(reason) = choice.finish_reason {
                if reason.is_empty() || reason.len() > MAX_PROVIDER_IDENTIFIER_BYTES {
                    return Err(ProviderError::MalformedResponse);
                }
                self.finish_reason = Some(reason);
            }
        }
        if let Some(usage) = chunk.usage {
            #[cfg(debug_assertions)]
            {
                self.diagnostic_stage = "validating chat usage";
            }
            if self.usage.is_some() {
                return Err(ProviderError::MalformedResponse);
            }
            self.usage = Some(self.validate_usage(usage)?);
        }
        Ok(events)
    }

    fn validate_chunk_identity(&mut self, chunk: &ChatChunk) -> Result<(), ProviderError> {
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
        #[cfg(debug_assertions)]
        {
            self.diagnostic_stage = "validating the delta role";
        }
        if let Some(role) = delta.role {
            if role != "assistant" {
                return Err(ProviderError::MalformedResponse);
            }
            self.role_seen = true;
        }
        #[cfg(debug_assertions)]
        {
            self.diagnostic_stage = "validating legacy function-call output";
        }
        if delta.function_call.is_some() {
            return Err(ProviderError::MalformedResponse);
        }
        #[cfg(debug_assertions)]
        {
            self.diagnostic_stage = "validating reasoning deltas";
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
        #[cfg(debug_assertions)]
        {
            self.diagnostic_stage = "validating reasoning details";
        }
        if delta
            .reasoning_details
            .as_ref()
            .is_some_and(|details| !details.is_empty())
        {
            return Err(ProviderError::MalformedResponse);
        }

        #[cfg(debug_assertions)]
        {
            self.diagnostic_stage = "recording text and tool deltas";
        }
        let mut events = Vec::new();
        if let Some(content) = delta.content.filter(|content| !content.is_empty()) {
            events.push(self.record_text_delta(content, false)?);
        }
        if let Some(refusal) = delta.refusal.filter(|refusal| !refusal.is_empty()) {
            events.push(self.record_text_delta(refusal, true)?);
        }
        if let Some(calls) = delta.tool_calls {
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
        let cached = usage
            .prompt_tokens_details
            .map_or(0, |details| details.cached_tokens);
        let reasoning = usage
            .completion_tokens_details
            .map_or(0, |details| details.reasoning_tokens);
        if usage.prompt_tokens == 0
            || usage.completion_tokens == 0
            || usage.prompt_tokens > u64::from(self.maximum_input_tokens)
            || usage.completion_tokens > u64::from(self.maximum_output_tokens)
            || usage.prompt_tokens > MAX_USAGE_TOKENS
            || usage.completion_tokens > MAX_USAGE_TOKENS
            || usage.prompt_tokens.checked_add(usage.completion_tokens) != Some(usage.total_tokens)
            || cached > usage.prompt_tokens
            || reasoning > usage.completion_tokens
        {
            return Err(ProviderError::MalformedResponse);
        }
        Ok(ProviderUsage {
            input_tokens: usage.prompt_tokens,
            cached_input_tokens: cached,
            cache_write_input_tokens: 0,
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
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptTokenDetails {
    #[serde(default)]
    cached_tokens: u64,
    #[serde(default, rename = "audio_tokens")]
    _audio_tokens: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompletionTokenDetails {
    #[serde(default)]
    reasoning_tokens: u64,
    #[serde(default, rename = "audio_tokens")]
    _audio_tokens: u64,
    #[serde(default, rename = "accepted_prediction_tokens")]
    _accepted_prediction_tokens: u64,
    #[serde(default, rename = "rejected_prediction_tokens")]
    _rejected_prediction_tokens: u64,
}

#[cfg(debug_assertions)]
fn diagnose_unknown_chunk_fields(value: &Value) {
    const KNOWN: &[&str] = &[
        "id",
        "object",
        "request_id",
        "created",
        "model",
        "choices",
        "usage",
        "system_fingerprint",
        "service_tier",
        "web_search",
        "cost",
    ];
    let Some(object) = value.as_object() else {
        return;
    };
    for key in object.keys().filter(|key| !KNOWN.contains(&key.as_str())) {
        let digest = Sha256::digest(key.as_bytes());
        let fingerprint = digest[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        eprintln!(
            "chat completions chunk has unknown field bytes={} fingerprint={fingerprint}",
            key.len()
        );
    }
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
mod tests;
