use serde::Deserialize;
use serde_json::Value;
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
const MAX_IGNORED_REASONING_BYTES: usize = 1024 * 1024;
const MAX_USAGE_TOKENS: u64 = 10_000_000;
const MAX_EVENT_DEPTH: usize = 32;
const MAX_EVENT_NODES: usize = 100_000;
const MAX_EVENT_COLLECTION_ITEMS: usize = 10_000;
const MAX_EVENT_OBJECT_FIELDS: usize = 256;
const MAX_EVENT_KEY_BYTES: usize = 128;
const MAX_SAFETY_RATINGS: usize = 32;

pub(super) struct GeminiDecoder {
    sse: SseDecoder,
    maximum_input_tokens: u32,
    maximum_output_tokens: u32,
    response_id: Option<String>,
    model_version: Option<String>,
    create_time: Option<String>,
    text: String,
    ignored_reasoning_bytes: usize,
    tool_calls: Vec<ProviderToolCall>,
    finish_reason: Option<String>,
    usage_components: GeminiUsageAccumulator,
    usage: Option<ProviderUsage>,
    provider_sequence: u64,
    done_marker_seen: bool,
    cost_trailer_seen: bool,
    #[cfg(debug_assertions)]
    diagnostic_stage: &'static str,
}

impl GeminiDecoder {
    pub(super) fn new(maximum_input_tokens: u32, maximum_output_tokens: u32) -> Self {
        Self {
            sse: SseDecoder::new(),
            maximum_input_tokens,
            maximum_output_tokens,
            response_id: None,
            model_version: None,
            create_time: None,
            text: String::new(),
            ignored_reasoning_bytes: 0,
            tool_calls: Vec::new(),
            finish_reason: None,
            usage_components: GeminiUsageAccumulator::default(),
            usage: None,
            provider_sequence: 0,
            done_marker_seen: false,
            cost_trailer_seen: false,
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
        let response_id = self.response_id.ok_or(ProviderError::IncompleteResponse)?;
        let usage = self.usage.ok_or(ProviderError::IncompleteResponse)?;
        let finish_reason = self
            .finish_reason
            .as_deref()
            .ok_or(ProviderError::IncompleteResponse)?;
        match finish_reason {
            "STOP" => {}
            "MAX_TOKENS" => return Err(ProviderError::IncompleteResponse),
            "SAFETY"
            | "RECITATION"
            | "LANGUAGE"
            | "BLOCKLIST"
            | "PROHIBITED_CONTENT"
            | "SPII"
            | "IMAGE_SAFETY"
            | "IMAGE_PROHIBITED_CONTENT"
            | "NO_IMAGE"
            | "IMAGE_RECITATION" => return Err(ProviderError::ProviderExecutionFailed),
            "MALFORMED_FUNCTION_CALL"
            | "UNEXPECTED_TOOL_CALL"
            | "TOO_MANY_TOOL_CALLS"
            | "IMAGE_OTHER"
            | "OTHER"
            | "FINISH_REASON_UNSPECIFIED" => {
                return Err(ProviderError::ProviderExecutionFailed);
            }
            _ => return Err(ProviderError::MalformedResponse),
        }
        if self.text.is_empty() && self.tool_calls.is_empty() {
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
                    refusal: false,
                },
            ));
        }
        output.extend(
            self.tool_calls
                .into_iter()
                .map(ProviderOutputItem::ToolCall),
        );
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
        if record.event.is_some() || self.done_marker_seen || self.cost_trailer_seen {
            return Err(ProviderError::MalformedResponse);
        }
        if record.data == b"[DONE]" {
            if self.finish_reason.is_none() || self.usage.is_none() {
                return Err(ProviderError::IncompleteResponse);
            }
            self.done_marker_seen = true;
            return Ok(Vec::new());
        }
        #[cfg(debug_assertions)]
        {
            self.diagnostic_stage = "decoding strict Gemini event JSON";
        }
        let value =
            parse_strict_value(&record.data).map_err(|_| ProviderError::MalformedResponse)?;
        let mut nodes = 0_usize;
        validate_event_value(&value, 0, &mut nodes)?;
        if value.get("type").and_then(Value::as_str) == Some("ping") {
            #[cfg(debug_assertions)]
            {
                self.diagnostic_stage = "validating the Zen Gemini cost trailer";
            }
            if self.finish_reason.is_none() || self.usage.is_none() {
                return Err(ProviderError::MalformedResponse);
            }
            validate_ping_value(&value)?;
            self.cost_trailer_seen = true;
            return Ok(Vec::new());
        }
        #[cfg(debug_assertions)]
        {
            self.diagnostic_stage = "decoding the Gemini event structure";
        }
        let event: GeminiEvent = serde_json::from_value(value).map_err(|error| {
            #[cfg(debug_assertions)]
            emit_unknown_field_diagnostic(&error);
            ProviderError::MalformedResponse
        })?;
        if event.candidates.is_none()
            && event.prompt_feedback.is_none()
            && event.usage_metadata.is_none()
            && event.model_version.is_none()
            && event.response_id.is_none()
        {
            return Err(ProviderError::MalformedResponse);
        }
        #[cfg(debug_assertions)]
        {
            self.diagnostic_stage = "validating Gemini response identity";
        }
        self.validate_identity(event.response_id, event.model_version, event.create_time)?;
        #[cfg(debug_assertions)]
        {
            self.diagnostic_stage = "validating Gemini prompt feedback";
        }
        self.validate_prompt_feedback(event.prompt_feedback)?;
        if let Some(usage) = event.usage_metadata {
            #[cfg(debug_assertions)]
            {
                self.diagnostic_stage = "validating Gemini usage";
            }
            self.record_usage(usage)?;
        }
        #[cfg(debug_assertions)]
        {
            self.diagnostic_stage = "validating Gemini candidates";
        }
        let Some(candidates) = event.candidates else {
            return Ok(Vec::new());
        };
        if candidates.len() > 1 {
            return Err(ProviderError::MalformedResponse);
        }
        let Some(candidate) = candidates.into_iter().next() else {
            return Ok(Vec::new());
        };
        self.process_candidate(candidate)
    }

    fn validate_identity(
        &mut self,
        response_id: Option<String>,
        model_version: Option<String>,
        create_time: Option<String>,
    ) -> Result<(), ProviderError> {
        if let Some(response_id) = response_id {
            validate_identifier(&response_id, MAX_PROVIDER_IDENTIFIER_BYTES)
                .map_err(|_| ProviderError::MalformedResponse)?;
            match &self.response_id {
                Some(expected) if expected != &response_id => {
                    return Err(ProviderError::MalformedResponse);
                }
                Some(_) => {}
                None => self.response_id = Some(response_id),
            }
        }
        if let Some(model_version) = model_version {
            validate_identifier(&model_version, MAX_PROVIDER_IDENTIFIER_BYTES)
                .map_err(|_| ProviderError::MalformedResponse)?;
            match &self.model_version {
                Some(expected) if expected != &model_version => {
                    return Err(ProviderError::MalformedResponse);
                }
                Some(_) => {}
                None => self.model_version = Some(model_version),
            }
        }
        if let Some(create_time) = create_time {
            if !is_valid_protobuf_timestamp(&create_time) {
                return Err(ProviderError::MalformedResponse);
            }
            match &self.create_time {
                Some(expected) if expected != &create_time => {
                    return Err(ProviderError::MalformedResponse);
                }
                Some(_) => {}
                None => self.create_time = Some(create_time),
            }
        }
        Ok(())
    }

    fn validate_prompt_feedback(
        &self,
        feedback: Option<GeminiPromptFeedback>,
    ) -> Result<(), ProviderError> {
        let Some(feedback) = feedback else {
            return Ok(());
        };
        validate_safety_ratings(feedback.safety_ratings.as_ref())?;
        if feedback.block_reason.is_some() || feedback.block_reason_message.is_some() {
            return Err(ProviderError::ProviderExecutionFailed);
        }
        Ok(())
    }

    fn process_candidate(
        &mut self,
        candidate: GeminiCandidate,
    ) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
        #[cfg(debug_assertions)]
        {
            self.diagnostic_stage = "validating a Gemini candidate";
        }
        if self.finish_reason.is_some()
            || candidate.index.is_some_and(|index| index != 0)
            || candidate.token_count.is_some_and(|tokens| {
                tokens > u64::from(self.maximum_output_tokens) || tokens > MAX_USAGE_TOKENS
            })
            || candidate
                .finish_message
                .as_ref()
                .is_some_and(|message| message.is_empty() || message.len() > MAX_DELTA_BYTES)
            || candidate.finish_message.is_some() && candidate.finish_reason.is_none()
            || candidate
                .avg_logprobs
                .is_some_and(|value| !value.is_finite())
            || candidate.citation_metadata.is_some()
            || candidate
                .grounding_attributions
                .as_ref()
                .is_some_and(|items| !items.is_empty())
            || candidate.grounding_metadata.is_some()
            || candidate.logprobs_result.is_some()
            || candidate.url_context_metadata.is_some()
        {
            return Err(ProviderError::MalformedResponse);
        }
        validate_safety_ratings(candidate.safety_ratings.as_ref())?;
        if let Some(reason) = candidate.finish_reason {
            if self
                .finish_reason
                .as_ref()
                .is_some_and(|existing| existing != &reason)
                || reason.is_empty()
                || reason.len() > MAX_PROVIDER_IDENTIFIER_BYTES
            {
                return Err(ProviderError::MalformedResponse);
            }
            self.finish_reason = Some(reason);
        }
        let Some(content) = candidate.content else {
            return Ok(Vec::new());
        };
        if content.role.as_deref() != Some("model") || content.parts.len() > MAX_TOOL_COUNT * 2 + 1
        {
            return Err(ProviderError::MalformedResponse);
        }
        let mut events = Vec::new();
        for part in content.parts {
            events.extend(self.process_part(part)?);
        }
        Ok(events)
    }

    fn process_part(&mut self, part: Value) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
        #[cfg(debug_assertions)]
        {
            self.diagnostic_stage = "validating a Gemini content part";
        }
        let object = part.as_object().ok_or(ProviderError::MalformedResponse)?;
        if object.contains_key("text") {
            let text_part: GeminiTextPart =
                serde_json::from_value(part).map_err(|_| ProviderError::MalformedResponse)?;
            if text_part.text.len() > MAX_DELTA_BYTES
                || text_part
                    .thought_signature
                    .as_ref()
                    .is_some_and(|signature| {
                        signature.is_empty() || signature.len() > MAX_IGNORED_REASONING_BYTES
                    })
            {
                return Err(ProviderError::ResponseLimitExceeded);
            }
            let thought = text_part.thought.unwrap_or(false);
            if !thought && text_part.thought_signature.is_some() {
                return Err(ProviderError::MalformedResponse);
            }
            if thought {
                self.ignored_reasoning_bytes = self
                    .ignored_reasoning_bytes
                    .checked_add(text_part.text.len())
                    .and_then(|bytes| {
                        bytes.checked_add(
                            text_part.thought_signature.as_ref().map_or(0, String::len),
                        )
                    })
                    .filter(|bytes| *bytes <= MAX_IGNORED_REASONING_BYTES)
                    .ok_or(ProviderError::ResponseLimitExceeded)?;
                return Ok(Vec::new());
            }
            if text_part.text.is_empty() {
                return Ok(Vec::new());
            }
            self.text
                .len()
                .checked_add(text_part.text.len())
                .filter(|bytes| *bytes <= MAX_ACCUMULATED_TEXT_BYTES)
                .ok_or(ProviderError::ResponseLimitExceeded)?;
            self.text.push_str(&text_part.text);
            let event = ProviderStreamEvent::TextDelta {
                provider_sequence: self.provider_sequence,
                output_index: 0,
                content_index: 0,
                delta: text_part.text,
                refusal: false,
            };
            self.provider_sequence = self
                .provider_sequence
                .checked_add(1)
                .ok_or(ProviderError::ResponseLimitExceeded)?;
            return Ok(vec![event]);
        }
        if object.contains_key("functionCall") {
            let call_part: GeminiFunctionCallPart =
                serde_json::from_value(part).map_err(|_| ProviderError::MalformedResponse)?;
            validate_tool_name(&call_part.function_call.name)
                .map_err(|_| ProviderError::MalformedResponse)?;
            if call_part
                .function_call
                .id
                .as_ref()
                .is_some_and(|id| validate_identifier(id, MAX_PROVIDER_CALL_ID_BYTES).is_err())
                || call_part
                    .thought_signature
                    .as_ref()
                    .is_some_and(|signature| {
                        signature.is_empty() || signature.len() > MAX_IGNORED_REASONING_BYTES
                    })
                || !call_part.function_call.args.is_object()
            {
                return Err(ProviderError::MalformedResponse);
            }
            let arguments = serde_json::to_string(&call_part.function_call.args)
                .map_err(|_| ProviderError::MalformedResponse)?;
            if arguments.len() > MAX_TOOL_ARGUMENT_BYTES || self.tool_calls.len() >= MAX_TOOL_COUNT
            {
                return Err(ProviderError::ResponseLimitExceeded);
            }
            let response_id = self
                .response_id
                .as_deref()
                .ok_or(ProviderError::MalformedResponse)?;
            let provider_call_id = gemini_call_id(response_id, self.tool_calls.len());
            validate_identifier(&provider_call_id, MAX_PROVIDER_CALL_ID_BYTES)
                .map_err(|_| ProviderError::MalformedResponse)?;
            self.tool_calls.push(ProviderToolCall {
                provider_item_id: None,
                provider_call_id,
                name: call_part.function_call.name,
                arguments,
                opaque_continuation: call_part.thought_signature,
            });
            return Ok(Vec::new());
        }
        Err(ProviderError::MalformedResponse)
    }

    fn record_usage(&mut self, usage: GeminiUsage) -> Result<(), ProviderError> {
        if usage
            .service_tier
            .as_deref()
            .is_some_and(|tier| tier != "standard")
        {
            return Err(ProviderError::MalformedResponse);
        }
        validate_usage_details(usage.prompt_tokens_details.as_ref())?;
        validate_usage_details(usage.cache_tokens_details.as_ref())?;
        validate_usage_details(usage.candidates_tokens_details.as_ref())?;
        validate_usage_details(usage.tool_use_prompt_tokens_details.as_ref())?;
        self.usage_components.merge(usage)?;

        let Some(input_tokens) = self.usage_components.prompt_token_count else {
            return Ok(());
        };
        let Some(visible_output_tokens) = self.usage_components.candidates_token_count else {
            return Ok(());
        };
        let Some(reported_total_tokens) = self.usage_components.total_token_count else {
            return Ok(());
        };
        let reasoning_output_tokens = self.usage_components.thoughts_token_count.unwrap_or(0);
        let output_tokens = visible_output_tokens
            .checked_add(reasoning_output_tokens)
            .ok_or(ProviderError::ResponseLimitExceeded)?;
        let cached_input_tokens = self
            .usage_components
            .cached_content_token_count
            .unwrap_or(0);
        let total_tokens = input_tokens
            .checked_add(output_tokens)
            .ok_or(ProviderError::ResponseLimitExceeded)?;
        if cached_input_tokens > input_tokens
            || reported_total_tokens != total_tokens
            || input_tokens > u64::from(self.maximum_input_tokens)
            || output_tokens > u64::from(self.maximum_output_tokens)
            || total_tokens > MAX_USAGE_TOKENS
            || self
                .usage_components
                .tool_use_prompt_token_count
                .is_some_and(|tokens| tokens > input_tokens)
        {
            return Err(ProviderError::MalformedResponse);
        }
        self.usage = Some(ProviderUsage {
            input_tokens,
            cached_input_tokens,
            cache_write_input_tokens: 0,
            output_tokens,
            reasoning_output_tokens,
            total_tokens,
        });
        Ok(())
    }
}

#[cfg(debug_assertions)]
fn emit_unknown_field_diagnostic(error: &serde_json::Error) {
    let message = error.to_string();
    let Some(rest) = message.strip_prefix("unknown field `") else {
        return;
    };
    let Some((field, _)) = rest.split_once('`') else {
        return;
    };
    let digest = Sha256::digest(field.as_bytes());
    let fingerprint = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    eprintln!(
        "Gemini decoder rejected unknown field bytes={} fingerprint={fingerprint}",
        field.len()
    );
}

fn gemini_call_id(response_id: &str, index: usize) -> String {
    let digest = Sha256::new()
        .chain_update(b"morons.dev/gemini-call/v1\0")
        .chain_update(response_id.as_bytes())
        .chain_update(index.to_be_bytes())
        .finalize();
    let mut id = String::from("gemini_call_");
    for byte in &digest[..16] {
        use std::fmt::Write as _;
        write!(id, "{byte:02x}").expect("writing into a String cannot fail");
    }
    id
}

#[derive(Default)]
struct GeminiUsageAccumulator {
    cached_content_token_count: Option<u64>,
    thoughts_token_count: Option<u64>,
    prompt_token_count: Option<u64>,
    candidates_token_count: Option<u64>,
    total_token_count: Option<u64>,
    tool_use_prompt_token_count: Option<u64>,
}

impl GeminiUsageAccumulator {
    fn merge(&mut self, usage: GeminiUsage) -> Result<(), ProviderError> {
        merge_stable(&mut self.prompt_token_count, usage.prompt_token_count)?;
        merge_stable(
            &mut self.cached_content_token_count,
            usage.cached_content_token_count,
        )?;
        merge_cumulative(
            &mut self.candidates_token_count,
            usage.candidates_token_count,
        )?;
        merge_cumulative(&mut self.thoughts_token_count, usage.thoughts_token_count)?;
        merge_cumulative(&mut self.total_token_count, usage.total_token_count)?;
        merge_cumulative(
            &mut self.tool_use_prompt_token_count,
            usage.tool_use_prompt_token_count,
        )?;
        for value in [
            self.cached_content_token_count,
            self.thoughts_token_count,
            self.prompt_token_count,
            self.candidates_token_count,
            self.total_token_count,
            self.tool_use_prompt_token_count,
        ]
        .into_iter()
        .flatten()
        {
            if value > MAX_USAGE_TOKENS {
                return Err(ProviderError::ResponseLimitExceeded);
            }
        }
        if let (Some(cached), Some(prompt)) =
            (self.cached_content_token_count, self.prompt_token_count)
            && cached > prompt
        {
            return Err(ProviderError::MalformedResponse);
        }
        Ok(())
    }
}

fn merge_stable(current: &mut Option<u64>, next: Option<u64>) -> Result<(), ProviderError> {
    match (*current, next) {
        (Some(current), Some(next)) if current != next => Err(ProviderError::MalformedResponse),
        (None, Some(next)) => {
            *current = Some(next);
            Ok(())
        }
        (Some(_), Some(_) | None) | (None, None) => Ok(()),
    }
}

fn merge_cumulative(current: &mut Option<u64>, next: Option<u64>) -> Result<(), ProviderError> {
    match (*current, next) {
        (Some(current), Some(next)) if next < current => Err(ProviderError::MalformedResponse),
        (_, Some(next)) => {
            *current = Some(next);
            Ok(())
        }
        (_, None) => Ok(()),
    }
}

fn validate_safety_ratings(ratings: Option<&Vec<Value>>) -> Result<(), ProviderError> {
    if ratings.is_some_and(|ratings| {
        ratings.len() > MAX_SAFETY_RATINGS || ratings.iter().any(|rating| !rating.is_object())
    }) {
        return Err(ProviderError::MalformedResponse);
    }
    Ok(())
}

fn validate_usage_details(
    details: Option<&Vec<GeminiModalityTokenCount>>,
) -> Result<(), ProviderError> {
    if details.is_some_and(|details| {
        details.len() > MAX_EVENT_COLLECTION_ITEMS
            || details.iter().any(|detail| {
                detail.modality.is_empty()
                    || detail.modality.len() > MAX_PROVIDER_IDENTIFIER_BYTES
                    || detail.token_count > MAX_USAGE_TOKENS
            })
    }) {
        return Err(ProviderError::MalformedResponse);
    }
    Ok(())
}

fn is_valid_protobuf_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes.len() > 30
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes.last() != Some(&b'Z')
        || [
            &bytes[0..4],
            &bytes[5..7],
            &bytes[8..10],
            &bytes[11..13],
            &bytes[14..16],
            &bytes[17..19],
        ]
        .into_iter()
        .flatten()
        .any(|byte| !byte.is_ascii_digit())
    {
        return false;
    }
    if bytes.len() > 20
        && (bytes.len() == 21
            || bytes[19] != b'.'
            || bytes[20..bytes.len() - 1]
                .iter()
                .any(|byte| !byte.is_ascii_digit()))
    {
        return false;
    }
    let pair = |start: usize| (bytes[start] - b'0') * 10 + (bytes[start + 1] - b'0');
    (1..=12).contains(&pair(5))
        && (1..=31).contains(&pair(8))
        && pair(11) <= 23
        && pair(14) <= 59
        && pair(17) <= 59
}

fn validate_ping_value(value: &Value) -> Result<(), ProviderError> {
    let object = value.as_object().ok_or(ProviderError::MalformedResponse)?;
    if object.len() != 2
        || object.get("type").and_then(Value::as_str) != Some("ping")
        || object
            .keys()
            .any(|key| !matches!(key.as_str(), "type" | "cost"))
    {
        return Err(ProviderError::MalformedResponse);
    }
    let cost = object.get("cost").ok_or(ProviderError::MalformedResponse)?;
    let parsed = match cost {
        Value::Number(value) => value.as_f64(),
        Value::String(value) if !value.is_empty() && value.len() <= 64 => value.parse().ok(),
        Value::String(_) | Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => {
            None
        }
    };
    if !parsed.is_some_and(|value: f64| value.is_finite() && (0.0..=1_000_000.0).contains(&value)) {
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
        .ok_or(ProviderError::ResponseLimitExceeded)?;
    if *nodes > MAX_EVENT_NODES {
        return Err(ProviderError::ResponseLimitExceeded);
    }
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
        Value::String(value) if value.len() > MAX_DELTA_BYTES => {
            return Err(ProviderError::ResponseLimitExceeded);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GeminiEvent {
    candidates: Option<Vec<GeminiCandidate>>,
    prompt_feedback: Option<GeminiPromptFeedback>,
    usage_metadata: Option<GeminiUsage>,
    model_version: Option<String>,
    response_id: Option<String>,
    create_time: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GeminiPromptFeedback {
    block_reason: Option<String>,
    block_reason_message: Option<String>,
    safety_ratings: Option<Vec<Value>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GeminiCandidate {
    content: Option<GeminiContent>,
    finish_reason: Option<String>,
    finish_message: Option<String>,
    safety_ratings: Option<Vec<Value>>,
    citation_metadata: Option<Value>,
    token_count: Option<u64>,
    grounding_attributions: Option<Vec<Value>>,
    grounding_metadata: Option<Value>,
    avg_logprobs: Option<f64>,
    logprobs_result: Option<Value>,
    url_context_metadata: Option<Value>,
    index: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GeminiContent {
    role: Option<String>,
    parts: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GeminiTextPart {
    text: String,
    thought: Option<bool>,
    thought_signature: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GeminiFunctionCallPart {
    function_call: GeminiFunctionCall,
    thought_signature: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GeminiFunctionCall {
    id: Option<String>,
    name: String,
    args: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GeminiUsage {
    cached_content_token_count: Option<u64>,
    thoughts_token_count: Option<u64>,
    prompt_token_count: Option<u64>,
    candidates_token_count: Option<u64>,
    total_token_count: Option<u64>,
    prompt_tokens_details: Option<Vec<GeminiModalityTokenCount>>,
    cache_tokens_details: Option<Vec<GeminiModalityTokenCount>>,
    candidates_tokens_details: Option<Vec<GeminiModalityTokenCount>>,
    tool_use_prompt_token_count: Option<u64>,
    tool_use_prompt_tokens_details: Option<Vec<GeminiModalityTokenCount>>,
    service_tier: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GeminiModalityTokenCount {
    modality: String,
    token_count: u64,
}

#[cfg(test)]
mod tests;
