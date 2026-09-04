use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::Value;

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
const MAX_IGNORED_REASONING_BYTES: usize = 1024 * 1024;
const MAX_USAGE_TOKENS: u64 = 10_000_000;
const MAX_EVENT_DEPTH: usize = 32;
const MAX_EVENT_NODES: usize = 100_000;
const MAX_EVENT_COLLECTION_ITEMS: usize = 10_000;
const MAX_EVENT_OBJECT_FIELDS: usize = 256;
const MAX_EVENT_KEY_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamState {
    AwaitingMessageStart,
    Content,
    MessageDelta,
    MessageStop,
}

enum ActiveBlock {
    Text {
        index: u32,
        text: String,
    },
    ToolUse {
        index: u32,
        id: String,
        name: String,
        arguments: String,
    },
    Thinking {
        index: u32,
        bytes: usize,
    },
}

impl ActiveBlock {
    const fn index(&self) -> u32 {
        match self {
            Self::Text { index, .. }
            | Self::ToolUse { index, .. }
            | Self::Thinking { index, .. } => *index,
        }
    }
}

pub(super) struct AnthropicMessagesDecoder {
    sse: SseDecoder,
    expected_model: &'static str,
    maximum_input_tokens: u32,
    maximum_output_tokens: u32,
    state: StreamState,
    response_id: Option<String>,
    input_tokens: Option<u64>,
    uncached_input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    output_tokens: Option<u64>,
    stop_reason: Option<String>,
    active_block: Option<ActiveBlock>,
    next_block_index: u32,
    output: Vec<ProviderOutputItem>,
    provider_sequence: u64,
    output_item_ids: BTreeSet<String>,
    #[cfg(debug_assertions)]
    diagnostic_stage: &'static str,
}

impl AnthropicMessagesDecoder {
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
            state: StreamState::AwaitingMessageStart,
            response_id: None,
            input_tokens: None,
            uncached_input_tokens: 0,
            cached_input_tokens: 0,
            cache_write_input_tokens: 0,
            output_tokens: None,
            stop_reason: None,
            active_block: None,
            next_block_index: 0,
            output: Vec::new(),
            provider_sequence: 0,
            output_item_ids: BTreeSet::new(),
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
            if let Some(event) = self.process_record(record)? {
                events.push(event);
            }
        }
        Ok(events)
    }

    pub(super) fn finish(mut self) -> Result<ProviderOutcome, ProviderError> {
        self.sse.finish()?;
        if self.state != StreamState::MessageStop || self.active_block.is_some() {
            return Err(ProviderError::IncompleteResponse);
        }
        let response_id = self
            .response_id
            .take()
            .ok_or(ProviderError::IncompleteResponse)?;
        let input_tokens = self.input_tokens.ok_or(ProviderError::IncompleteResponse)?;
        let output_tokens = self
            .output_tokens
            .ok_or(ProviderError::IncompleteResponse)?;
        let stop_reason = self
            .stop_reason
            .as_deref()
            .ok_or(ProviderError::IncompleteResponse)?;
        let has_tool_calls = self
            .output
            .iter()
            .any(|item| matches!(item, ProviderOutputItem::ToolCall(_)));
        match stop_reason {
            "end_turn" | "stop_sequence" if !has_tool_calls => {}
            "tool_use" if has_tool_calls => {}
            "max_tokens" | "model_context_window_exceeded" => {
                return Err(ProviderError::IncompleteResponse);
            }
            "refusal" => return Err(ProviderError::ProviderExecutionFailed),
            _ => return Err(ProviderError::MalformedResponse),
        }
        if self.output.is_empty() {
            return Err(ProviderError::MalformedResponse);
        }
        for item in &mut self.output {
            if let ProviderOutputItem::AssistantMessage(message) = item {
                message.phase = Some(if has_tool_calls {
                    ProviderMessagePhase::Commentary
                } else {
                    ProviderMessagePhase::FinalAnswer
                });
            }
        }
        let total_tokens = input_tokens
            .checked_add(output_tokens)
            .ok_or(ProviderError::ResponseLimitExceeded)?;
        Ok(ProviderOutcome {
            provider_response_id: response_id,
            output: self.output,
            usage: ProviderUsage {
                input_tokens,
                cached_input_tokens: self.cached_input_tokens,
                cache_write_input_tokens: self.cache_write_input_tokens,
                output_tokens,
                reasoning_output_tokens: 0,
                total_tokens,
            },
        })
    }

    fn process_record(
        &mut self,
        record: SseRecord,
    ) -> Result<Option<ProviderStreamEvent>, ProviderError> {
        #[cfg(debug_assertions)]
        {
            self.diagnostic_stage = "decoding strict event JSON";
        }
        let value =
            parse_strict_value(&record.data).map_err(|_| ProviderError::MalformedResponse)?;
        let mut nodes = 0_usize;
        #[cfg(debug_assertions)]
        {
            self.diagnostic_stage = "validating event resource bounds";
        }
        validate_event_value(&value, 0, &mut nodes)?;
        #[cfg(debug_assertions)]
        {
            self.diagnostic_stage = "validating the event envelope";
        }
        let event_type = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or(ProviderError::MalformedResponse)?;
        if record.event.as_deref() != Some(event_type) {
            return Err(ProviderError::MalformedResponse);
        }
        match event_type {
            "ping" => {
                #[cfg(debug_assertions)]
                {
                    self.diagnostic_stage = "validating a ping event";
                }
                validate_ping(value)?;
                Ok(None)
            }
            "message_start" => {
                #[cfg(debug_assertions)]
                {
                    self.diagnostic_stage = "validating a message-start event";
                }
                self.message_start(value)?;
                Ok(None)
            }
            "content_block_start" => {
                #[cfg(debug_assertions)]
                {
                    self.diagnostic_stage = "validating a content-block-start event";
                }
                self.content_block_start(value)?;
                Ok(None)
            }
            "content_block_delta" => {
                #[cfg(debug_assertions)]
                {
                    self.diagnostic_stage = "validating a content-block-delta event";
                }
                self.content_block_delta(value)
            }
            "content_block_stop" => {
                #[cfg(debug_assertions)]
                {
                    self.diagnostic_stage = "validating a content-block-stop event";
                }
                self.content_block_stop(value)?;
                Ok(None)
            }
            "message_delta" => {
                #[cfg(debug_assertions)]
                {
                    self.diagnostic_stage = "validating a message-delta event";
                }
                self.message_delta(value)?;
                Ok(None)
            }
            "message_stop" => {
                #[cfg(debug_assertions)]
                {
                    self.diagnostic_stage = "validating a message-stop event";
                }
                self.message_stop(value)?;
                Ok(None)
            }
            "error" => Err(ProviderError::ProviderExecutionFailed),
            _ => Err(ProviderError::MalformedResponse),
        }
    }

    fn message_start(&mut self, value: Value) -> Result<(), ProviderError> {
        if self.state != StreamState::AwaitingMessageStart {
            return Err(ProviderError::MalformedResponse);
        }
        let event: MessageStartEvent =
            serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
        validate_identifier(&event.message.id, MAX_PROVIDER_IDENTIFIER_BYTES)?;
        if event.message.message_type != "message"
            || event.message.role != "assistant"
            || event.message.model != self.expected_model
            || !event.message.content.is_empty()
            || event.message.container.is_some()
            || event.message.stop_details.is_some()
            || event.message.stop_reason.is_some()
            || event.message.stop_sequence.is_some()
        {
            return Err(ProviderError::MalformedResponse);
        }
        let cached_input_tokens = event.message.usage.cache_read_input_tokens.unwrap_or(0);
        let cache_write_input_tokens = event.message.usage.cache_creation_input_tokens.unwrap_or(0);
        validate_usage_metadata(
            event.message.usage.cache_creation.as_ref(),
            cache_write_input_tokens,
            event.message.usage.server_tool_use.as_ref(),
            event.message.usage.service_tier.as_deref(),
            event.message.usage.inference_geo.as_deref(),
        )?;
        let input_tokens = event
            .message
            .usage
            .input_tokens
            .checked_add(cached_input_tokens)
            .and_then(|tokens| tokens.checked_add(cache_write_input_tokens))
            .ok_or(ProviderError::ResponseLimitExceeded)?;
        if input_tokens > u64::from(self.maximum_input_tokens)
            || input_tokens > MAX_USAGE_TOKENS
            || event.message.usage.output_tokens > u64::from(self.maximum_output_tokens)
            || event.message.usage.output_tokens > MAX_USAGE_TOKENS
        {
            return Err(ProviderError::MalformedResponse);
        }
        self.response_id = Some(event.message.id);
        self.input_tokens = Some(input_tokens);
        self.uncached_input_tokens = event.message.usage.input_tokens;
        self.cached_input_tokens = cached_input_tokens;
        self.cache_write_input_tokens = cache_write_input_tokens;
        self.state = StreamState::Content;
        Ok(())
    }

    fn content_block_start(&mut self, value: Value) -> Result<(), ProviderError> {
        if self.state != StreamState::Content || self.active_block.is_some() {
            return Err(ProviderError::MalformedResponse);
        }
        let event: ContentBlockStartEvent =
            serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
        if event.index != self.next_block_index || event.index as usize >= MAX_TOOL_COUNT * 2 {
            return Err(ProviderError::MalformedResponse);
        }
        let block_type = event
            .content_block
            .get("type")
            .and_then(Value::as_str)
            .ok_or(ProviderError::MalformedResponse)?;
        self.active_block = Some(match block_type {
            "text" => {
                let block: TextBlockStart = serde_json::from_value(event.content_block)
                    .map_err(|_| ProviderError::MalformedResponse)?;
                if !block.text.is_empty()
                    || block
                        .citations
                        .as_ref()
                        .is_some_and(|citations| !citations.is_empty())
                {
                    return Err(ProviderError::MalformedResponse);
                }
                ActiveBlock::Text {
                    index: event.index,
                    text: String::new(),
                }
            }
            "tool_use" => {
                let block: ToolUseBlockStart = serde_json::from_value(event.content_block)
                    .map_err(|_| ProviderError::MalformedResponse)?;
                validate_identifier(&block.id, MAX_PROVIDER_CALL_ID_BYTES)?;
                validate_tool_name(&block.name)?;
                if block
                    .input
                    .as_object()
                    .is_none_or(|input| !input.is_empty())
                    || block
                        .caller
                        .as_ref()
                        .is_some_and(|caller| caller.caller_type != "direct")
                {
                    return Err(ProviderError::MalformedResponse);
                }
                ActiveBlock::ToolUse {
                    index: event.index,
                    id: block.id,
                    name: block.name,
                    arguments: String::new(),
                }
            }
            "thinking" => {
                let block: ThinkingBlockStart = serde_json::from_value(event.content_block)
                    .map_err(|_| ProviderError::MalformedResponse)?;
                if !block.thinking.is_empty()
                    || block
                        .signature
                        .as_ref()
                        .is_some_and(|value| !value.is_empty())
                {
                    return Err(ProviderError::MalformedResponse);
                }
                ActiveBlock::Thinking {
                    index: event.index,
                    bytes: 0,
                }
            }
            "redacted_thinking" => {
                let block: RedactedThinkingBlockStart = serde_json::from_value(event.content_block)
                    .map_err(|_| ProviderError::MalformedResponse)?;
                if block.data.is_empty() || block.data.len() > MAX_IGNORED_REASONING_BYTES {
                    return Err(ProviderError::MalformedResponse);
                }
                ActiveBlock::Thinking {
                    index: event.index,
                    bytes: block.data.len(),
                }
            }
            _ => return Err(ProviderError::MalformedResponse),
        });
        Ok(())
    }

    fn content_block_delta(
        &mut self,
        value: Value,
    ) -> Result<Option<ProviderStreamEvent>, ProviderError> {
        if self.state != StreamState::Content {
            return Err(ProviderError::MalformedResponse);
        }
        let event: ContentBlockDeltaEvent =
            serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
        let block = self
            .active_block
            .as_mut()
            .filter(|block| block.index() == event.index)
            .ok_or(ProviderError::MalformedResponse)?;
        let delta_type = event
            .delta
            .get("type")
            .and_then(Value::as_str)
            .ok_or(ProviderError::MalformedResponse)?;
        match (block, delta_type) {
            (ActiveBlock::Text { text, .. }, "text_delta") => {
                let delta: TextDelta = serde_json::from_value(event.delta)
                    .map_err(|_| ProviderError::MalformedResponse)?;
                append_bounded(text, &delta.text, MAX_ACCUMULATED_TEXT_BYTES)?;
                let provider_sequence = self.provider_sequence;
                self.provider_sequence = self
                    .provider_sequence
                    .checked_add(1)
                    .ok_or(ProviderError::ResponseLimitExceeded)?;
                Ok(Some(ProviderStreamEvent::TextDelta {
                    provider_sequence,
                    output_index: event.index,
                    content_index: 0,
                    delta: delta.text,
                    refusal: false,
                }))
            }
            (ActiveBlock::ToolUse { arguments, .. }, "input_json_delta") => {
                let delta: InputJsonDelta = serde_json::from_value(event.delta)
                    .map_err(|_| ProviderError::MalformedResponse)?;
                append_bounded(arguments, &delta.partial_json, MAX_TOOL_ARGUMENT_BYTES)?;
                Ok(None)
            }
            (ActiveBlock::Thinking { bytes, .. }, "thinking_delta") => {
                let delta: ThinkingDelta = serde_json::from_value(event.delta)
                    .map_err(|_| ProviderError::MalformedResponse)?;
                add_ignored_bytes(bytes, delta.thinking.len())?;
                Ok(None)
            }
            (ActiveBlock::Thinking { bytes, .. }, "signature_delta") => {
                let delta: SignatureDelta = serde_json::from_value(event.delta)
                    .map_err(|_| ProviderError::MalformedResponse)?;
                add_ignored_bytes(bytes, delta.signature.len())?;
                Ok(None)
            }
            _ => Err(ProviderError::MalformedResponse),
        }
    }

    fn content_block_stop(&mut self, value: Value) -> Result<(), ProviderError> {
        if self.state != StreamState::Content {
            return Err(ProviderError::MalformedResponse);
        }
        let event: ContentBlockStopEvent =
            serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
        let block = self
            .active_block
            .take()
            .filter(|block| block.index() == event.index)
            .ok_or(ProviderError::MalformedResponse)?;
        match block {
            ActiveBlock::Text { index, text } => {
                if !text.is_empty() {
                    let item_id = self.content_item_id(index)?;
                    self.output.push(ProviderOutputItem::AssistantMessage(
                        ProviderAssistantMessage {
                            provider_item_id: item_id,
                            phase: None,
                            text,
                            refusal: false,
                        },
                    ));
                }
            }
            ActiveBlock::ToolUse {
                id,
                name,
                arguments,
                ..
            } => {
                let arguments = if arguments.is_empty() {
                    "{}".to_owned()
                } else {
                    arguments
                };
                let decoded = parse_strict_value(arguments.as_bytes())
                    .map_err(|_| ProviderError::MalformedResponse)?;
                if !decoded.is_object()
                    || self
                        .output
                        .iter()
                        .filter(|item| matches!(item, ProviderOutputItem::ToolCall(_)))
                        .count()
                        >= MAX_TOOL_COUNT
                {
                    return Err(ProviderError::MalformedResponse);
                }
                self.output
                    .push(ProviderOutputItem::ToolCall(ProviderToolCall {
                        provider_item_id: None,
                        provider_call_id: id,
                        name,
                        arguments,
                    }));
            }
            ActiveBlock::Thinking { .. } => {}
        }
        self.next_block_index = self
            .next_block_index
            .checked_add(1)
            .ok_or(ProviderError::ResponseLimitExceeded)?;
        Ok(())
    }

    fn message_delta(&mut self, value: Value) -> Result<(), ProviderError> {
        if self.state != StreamState::Content || self.active_block.is_some() {
            return Err(ProviderError::MalformedResponse);
        }
        let event: MessageDeltaEvent =
            serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
        let (uncached_input_tokens, cached_input_tokens, cache_write_input_tokens, input_tokens) =
            validate_delta_usage(
                &event.usage,
                self.uncached_input_tokens,
                self.cached_input_tokens,
                self.cache_write_input_tokens,
                self.maximum_input_tokens,
            )?;
        if event.delta.container.is_some()
            || (event.delta.stop_details.is_some() && event.delta.stop_reason != "refusal")
            || event.delta.stop_reason.is_empty()
            || event.delta.stop_reason.len() > MAX_PROVIDER_IDENTIFIER_BYTES
            || event.delta.stop_sequence.as_ref().is_some_and(|sequence| {
                sequence.is_empty() || sequence.len() > MAX_PROVIDER_IDENTIFIER_BYTES
            })
            || input_tokens == 0
            || event.usage.output_tokens == 0
            || event.usage.output_tokens > u64::from(self.maximum_output_tokens)
            || event.usage.output_tokens > MAX_USAGE_TOKENS
        {
            return Err(ProviderError::MalformedResponse);
        }
        self.uncached_input_tokens = uncached_input_tokens;
        self.cached_input_tokens = cached_input_tokens;
        self.cache_write_input_tokens = cache_write_input_tokens;
        self.input_tokens = Some(input_tokens);
        self.stop_reason = Some(event.delta.stop_reason);
        self.output_tokens = Some(event.usage.output_tokens);
        self.state = StreamState::MessageDelta;
        Ok(())
    }

    fn message_stop(&mut self, value: Value) -> Result<(), ProviderError> {
        if self.state != StreamState::MessageDelta {
            return Err(ProviderError::MalformedResponse);
        }
        let event: MessageStopEvent =
            serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
        if let Some(cost) = event.cost
            && !cost.is_valid()
        {
            return Err(ProviderError::MalformedResponse);
        }
        self.state = StreamState::MessageStop;
        Ok(())
    }

    fn content_item_id(&mut self, index: u32) -> Result<String, ProviderError> {
        let response_id = self
            .response_id
            .as_deref()
            .ok_or(ProviderError::MalformedResponse)?;
        let item_id = format!("{response_id}:{index}");
        validate_identifier(&item_id, MAX_PROVIDER_IDENTIFIER_BYTES)?;
        if !self.output_item_ids.insert(item_id.clone()) {
            return Err(ProviderError::MalformedResponse);
        }
        Ok(item_id)
    }
}

fn append_bounded(target: &mut String, delta: &str, maximum: usize) -> Result<(), ProviderError> {
    if delta.len() > MAX_DELTA_BYTES
        || target
            .len()
            .checked_add(delta.len())
            .is_none_or(|length| length > maximum)
    {
        return Err(ProviderError::ResponseLimitExceeded);
    }
    target.push_str(delta);
    Ok(())
}

fn add_ignored_bytes(total: &mut usize, bytes: usize) -> Result<(), ProviderError> {
    if bytes > MAX_DELTA_BYTES {
        return Err(ProviderError::ResponseLimitExceeded);
    }
    *total = total
        .checked_add(bytes)
        .filter(|total| *total <= MAX_IGNORED_REASONING_BYTES)
        .ok_or(ProviderError::ResponseLimitExceeded)?;
    Ok(())
}

fn validate_ping(value: Value) -> Result<(), ProviderError> {
    let ping: PingEvent =
        serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
    if let Some(cost) = ping.cost
        && !cost.is_valid()
    {
        return Err(ProviderError::MalformedResponse);
    }
    Ok(())
}

fn validate_usage_metadata(
    cache_creation: Option<&CacheCreation>,
    cache_creation_input_tokens: u64,
    server_tool_use: Option<&ServerToolUsage>,
    service_tier: Option<&str>,
    inference_geo: Option<&str>,
) -> Result<(), ProviderError> {
    if cache_creation.is_some_and(|cache| {
        cache
            .ephemeral_1h_input_tokens
            .checked_add(cache.ephemeral_5m_input_tokens)
            != Some(cache_creation_input_tokens)
    }) || server_tool_use
        .is_some_and(|usage| usage.web_fetch_requests != 0 || usage.web_search_requests != 0)
        || service_tier.is_some_and(|tier| !matches!(tier, "standard" | "priority" | "batch"))
        || inference_geo.is_some_and(|geo| {
            geo.is_empty()
                || geo.len() > 64
                || geo.bytes().any(|byte| !(0x21..=0x7e).contains(&byte))
        })
    {
        return Err(ProviderError::MalformedResponse);
    }
    Ok(())
}

fn validate_delta_usage(
    usage: &DeltaUsage,
    previous_uncached_input_tokens: u64,
    previous_cached_input_tokens: u64,
    previous_cache_write_input_tokens: u64,
    maximum_input_tokens: u32,
) -> Result<(u64, u64, u64, u64), ProviderError> {
    let uncached_input_tokens = usage.input_tokens.unwrap_or(previous_uncached_input_tokens);
    let cached_input_tokens = usage
        .cache_read_input_tokens
        .unwrap_or(previous_cached_input_tokens);
    let cache_write_input_tokens = usage
        .cache_creation_input_tokens
        .unwrap_or(previous_cache_write_input_tokens);
    if uncached_input_tokens < previous_uncached_input_tokens
        || cached_input_tokens < previous_cached_input_tokens
        || cache_write_input_tokens < previous_cache_write_input_tokens
        || usage
            .server_tool_use
            .as_ref()
            .is_some_and(|usage| usage.web_fetch_requests != 0 || usage.web_search_requests != 0)
    {
        return Err(ProviderError::MalformedResponse);
    }
    validate_input_token_count(uncached_input_tokens, maximum_input_tokens)?;
    validate_input_token_count(cached_input_tokens, maximum_input_tokens)?;
    validate_input_token_count(cache_write_input_tokens, maximum_input_tokens)?;
    let input_tokens = uncached_input_tokens
        .checked_add(cached_input_tokens)
        .and_then(|tokens| tokens.checked_add(cache_write_input_tokens))
        .ok_or(ProviderError::ResponseLimitExceeded)?;
    validate_input_token_count(input_tokens, maximum_input_tokens)?;
    Ok((
        uncached_input_tokens,
        cached_input_tokens,
        cache_write_input_tokens,
        input_tokens,
    ))
}

fn validate_input_token_count(tokens: u64, maximum_input_tokens: u32) -> Result<(), ProviderError> {
    if tokens > u64::from(maximum_input_tokens) || tokens > MAX_USAGE_TOKENS {
        return Err(ProviderError::MalformedResponse);
    }
    Ok(())
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageStartEvent {
    #[serde(rename = "type")]
    _event_type: String,
    message: StartMessage,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartMessage {
    id: String,
    #[serde(rename = "type")]
    message_type: String,
    role: String,
    model: String,
    content: Vec<Value>,
    #[serde(default)]
    container: Option<Value>,
    #[serde(default)]
    stop_details: Option<Value>,
    stop_reason: Option<String>,
    stop_sequence: Option<String>,
    usage: StartUsage,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartUsage {
    input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation: Option<CacheCreation>,
    #[serde(default)]
    server_tool_use: Option<ServerToolUsage>,
    #[serde(default)]
    service_tier: Option<String>,
    #[serde(default)]
    inference_geo: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheCreation {
    ephemeral_1h_input_tokens: u64,
    ephemeral_5m_input_tokens: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerToolUsage {
    web_fetch_requests: u64,
    web_search_requests: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContentBlockStartEvent {
    #[serde(rename = "type")]
    _event_type: String,
    index: u32,
    content_block: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextBlockStart {
    #[serde(rename = "type")]
    _block_type: String,
    text: String,
    #[serde(default)]
    citations: Option<Vec<Value>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolUseBlockStart {
    #[serde(rename = "type")]
    _block_type: String,
    id: String,
    name: String,
    input: Value,
    #[serde(default)]
    caller: Option<ToolCaller>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolCaller {
    #[serde(rename = "type")]
    caller_type: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThinkingBlockStart {
    #[serde(rename = "type")]
    _block_type: String,
    thinking: String,
    signature: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RedactedThinkingBlockStart {
    #[serde(rename = "type")]
    _block_type: String,
    data: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContentBlockDeltaEvent {
    #[serde(rename = "type")]
    _event_type: String,
    index: u32,
    delta: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextDelta {
    #[serde(rename = "type")]
    _delta_type: String,
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputJsonDelta {
    #[serde(rename = "type")]
    _delta_type: String,
    partial_json: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThinkingDelta {
    #[serde(rename = "type")]
    _delta_type: String,
    thinking: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignatureDelta {
    #[serde(rename = "type")]
    _delta_type: String,
    signature: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContentBlockStopEvent {
    #[serde(rename = "type")]
    _event_type: String,
    index: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageDeltaEvent {
    #[serde(rename = "type")]
    _event_type: String,
    delta: MessageDelta,
    usage: DeltaUsage,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageDelta {
    #[serde(default)]
    container: Option<Value>,
    #[serde(default)]
    stop_details: Option<Value>,
    stop_reason: String,
    stop_sequence: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeltaUsage {
    output_tokens: u64,
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    server_tool_use: Option<ServerToolUsage>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageStopEvent {
    #[serde(rename = "type")]
    _event_type: String,
    cost: Option<Cost>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PingEvent {
    #[serde(rename = "type")]
    _event_type: String,
    cost: Option<Cost>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Cost {
    Number(serde_json::Number),
    String(String),
}

impl Cost {
    fn is_valid(&self) -> bool {
        let value = match self {
            Self::Number(value) => value.as_f64(),
            Self::String(value) if !value.is_empty() && value.len() <= 64 => value.parse().ok(),
            Self::String(_) => None,
        };
        value.is_some_and(|value: f64| value.is_finite() && (0.0..=1_000_000.0).contains(&value))
    }
}

#[cfg(test)]
mod tests;
