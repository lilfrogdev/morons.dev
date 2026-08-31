use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::{
    ProviderAssistantMessage, ProviderError, ProviderOutcome, ProviderOutputItem,
    ProviderReasoning, ProviderStreamEvent, ProviderToolCall, ProviderUsage,
    json::parse_strict_value,
    request::{
        MAX_PROVIDER_CALL_ID_BYTES, MAX_TOOL_ARGUMENT_BYTES, MAX_TOOL_COUNT,
        validate_identifier as validate_request_identifier,
        validate_tool_name as validate_request_tool_name,
    },
    sse::{SseDecoder, SseRecord},
};

const MAX_PROVIDER_IDENTIFIER_BYTES: usize = 128;
const MAX_OUTPUT_ITEMS: usize = 128;
const MAX_MESSAGE_CONTENT_PARTS: usize = 64;
const MAX_ACCUMULATED_TEXT_BYTES: usize = 1024 * 1024;
const MAX_DELTA_BYTES: usize = 64 * 1024;
const MAX_USAGE_TOKENS: u64 = 10_000_000;
const MAX_REASONING_SUMMARIES: usize = 64;
const MAX_ENCRYPTED_REASONING_BYTES: usize = 512 * 1024;
const MAX_EVENT_DEPTH: usize = 32;
const MAX_EVENT_NODES: usize = 100_000;
const MAX_EVENT_COLLECTION_ITEMS: usize = 10_000;
const MAX_EVENT_OBJECT_FIELDS: usize = 256;
const MAX_EVENT_KEY_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamState {
    AwaitingCreated,
    Active,
    Terminal,
    DoneMarker,
}

struct DeltaAccumulator {
    item_id: String,
    value: String,
    refusal: bool,
}

#[cfg(test)]
pub(super) struct ResponsesDiagnostic {
    pub(super) event_type: Option<String>,
    pub(super) sequence_number: Option<u64>,
    pub(super) stage: &'static str,
}

pub(super) struct ResponsesDecoder {
    sse: SseDecoder,
    expected_model: &'static str,
    maximum_input_tokens: u32,
    maximum_output_tokens: u32,
    expected_sequence: u64,
    state: StreamState,
    response_id: Option<String>,
    text_deltas: BTreeMap<(u32, u32), DeltaAccumulator>,
    argument_deltas: BTreeMap<u32, DeltaAccumulator>,
    terminal: Option<Result<ProviderOutcome, ProviderError>>,
    #[cfg(test)]
    diagnostic_event_type: Option<String>,
    #[cfg(test)]
    diagnostic_sequence_number: Option<u64>,
    #[cfg(test)]
    diagnostic_stage: &'static str,
}

impl ResponsesDecoder {
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
            expected_sequence: 0,
            state: StreamState::AwaitingCreated,
            response_id: None,
            text_deltas: BTreeMap::new(),
            argument_deltas: BTreeMap::new(),
            terminal: None,
            #[cfg(test)]
            diagnostic_event_type: None,
            #[cfg(test)]
            diagnostic_sequence_number: None,
            #[cfg(test)]
            diagnostic_stage: "awaiting the first SSE record",
        }
    }

    #[cfg(test)]
    pub(super) fn diagnostic(&self) -> ResponsesDiagnostic {
        ResponsesDiagnostic {
            event_type: self.diagnostic_event_type.clone(),
            sequence_number: self.diagnostic_sequence_number,
            stage: self.diagnostic_stage,
        }
    }

    pub(super) fn push(&mut self, chunk: &[u8]) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
        let records = self.sse.push(chunk)?;
        let mut events = Vec::new();
        for record in records {
            #[cfg(test)]
            {
                self.diagnostic_event_type = None;
                self.diagnostic_sequence_number = None;
                self.diagnostic_stage = "decoding an SSE record";
            }
            if let Some(event) = self.process_record(record)? {
                events.push(event);
            }
        }
        Ok(events)
    }

    pub(super) fn finish(self) -> Result<ProviderOutcome, ProviderError> {
        self.sse.finish()?;
        match self.state {
            StreamState::Terminal | StreamState::DoneMarker => {
                self.terminal.ok_or(ProviderError::IncompleteResponse)?
            }
            StreamState::AwaitingCreated | StreamState::Active => {
                Err(ProviderError::IncompleteResponse)
            }
        }
    }

    fn process_record(
        &mut self,
        record: SseRecord,
    ) -> Result<Option<ProviderStreamEvent>, ProviderError> {
        if record.event.as_deref() == Some("ping") {
            #[cfg(test)]
            {
                self.diagnostic_event_type = record.event.clone();
                self.diagnostic_stage = "validating a transport ping";
            }
            validate_ping_record(&record.data)?;
            return Ok(None);
        }
        if record.data == b"[DONE]" {
            #[cfg(test)]
            {
                self.diagnostic_event_type = record.event.clone();
                self.diagnostic_stage = "validating the done marker";
            }
            if record.event.is_some() || self.state != StreamState::Terminal {
                return Err(ProviderError::MalformedResponse);
            }
            self.state = StreamState::DoneMarker;
            return Ok(None);
        }
        if matches!(self.state, StreamState::Terminal | StreamState::DoneMarker) {
            #[cfg(test)]
            {
                self.diagnostic_event_type = record.event.clone();
                self.diagnostic_stage = "rejecting a record after the terminal response";
            }
            return Err(ProviderError::MalformedResponse);
        }
        #[cfg(test)]
        {
            self.diagnostic_stage = "decoding event JSON";
        }
        let value =
            parse_strict_value(&record.data).map_err(|_| ProviderError::MalformedResponse)?;
        #[cfg(test)]
        {
            self.diagnostic_stage = "validating event structure";
        }
        let mut event_nodes = 0;
        validate_event_value(&value, 0, &mut event_nodes)?;
        let envelope: EventEnvelope =
            serde_json::from_value(value.clone()).map_err(|_| ProviderError::MalformedResponse)?;
        validate_event_type(&envelope.event_type)?;
        #[cfg(test)]
        {
            self.diagnostic_event_type = Some(envelope.event_type.clone());
            self.diagnostic_sequence_number = Some(envelope.sequence_number);
            self.diagnostic_stage = "validating event envelope";
        }
        if record
            .event
            .as_deref()
            .is_some_and(|event| event != envelope.event_type)
        {
            return Err(ProviderError::MalformedResponse);
        }
        if envelope.sequence_number != self.expected_sequence {
            return Err(ProviderError::MalformedResponse);
        }
        self.expected_sequence = self
            .expected_sequence
            .checked_add(1)
            .ok_or(ProviderError::ResponseLimitExceeded)?;

        match envelope.event_type.as_str() {
            "response.created" => {
                if self.state != StreamState::AwaitingCreated {
                    return Err(ProviderError::MalformedResponse);
                }
                let event: LifecycleEvent =
                    serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
                self.validate_lifecycle_response(&event.response, &["in_progress", "queued"])?;
                self.response_id = Some(event.response.id);
                self.state = StreamState::Active;
                Ok(None)
            }
            "response.queued" => {
                self.require_active()?;
                let event: LifecycleEvent =
                    serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
                self.validate_matching_lifecycle_response(&event.response, &["queued"])?;
                Ok(None)
            }
            "response.in_progress" => {
                self.require_active()?;
                let event: LifecycleEvent =
                    serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
                self.validate_matching_lifecycle_response(&event.response, &["in_progress"])?;
                Ok(None)
            }
            "response.output_text.delta" => {
                self.require_active()?;
                #[cfg(test)]
                {
                    self.diagnostic_stage = "validating an output-text delta";
                }
                let event: TextDeltaEvent =
                    serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
                if !event.logprobs.is_empty() {
                    return Err(ProviderError::MalformedResponse);
                }
                self.record_text_delta(
                    event.sequence_number,
                    event.output_index,
                    event.content_index,
                    event.item_id,
                    event.delta,
                    false,
                )
                .map(Some)
            }
            "response.refusal.delta" => {
                self.require_active()?;
                #[cfg(test)]
                {
                    self.diagnostic_stage = "validating a refusal delta";
                }
                let event: RefusalDeltaEvent =
                    serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
                self.record_text_delta(
                    event.sequence_number,
                    event.output_index,
                    event.content_index,
                    event.item_id,
                    event.delta,
                    true,
                )
                .map(Some)
            }
            "response.function_call_arguments.delta" => {
                self.require_active()?;
                #[cfg(test)]
                {
                    self.diagnostic_stage = "validating a function-arguments delta";
                }
                let event: FunctionArgumentsDeltaEvent =
                    serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
                self.record_argument_delta(event.output_index, event.item_id, event.delta)?;
                Ok(None)
            }
            "response.completed" => {
                self.require_active()?;
                #[cfg(test)]
                {
                    self.diagnostic_stage = "decoding the completed response";
                }
                let event: CompletedEvent =
                    serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
                #[cfg(test)]
                {
                    self.diagnostic_stage = "validating the completed response";
                }
                let outcome = self.parse_completed(event.response)?;
                self.terminal = Some(Ok(outcome));
                self.state = StreamState::Terminal;
                Ok(None)
            }
            "response.failed" => {
                self.require_active()?;
                let event: LifecycleEvent =
                    serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
                self.validate_matching_lifecycle_response(&event.response, &["failed"])?;
                self.terminal = Some(Err(ProviderError::ProviderExecutionFailed));
                self.state = StreamState::Terminal;
                Ok(None)
            }
            "error" => {
                self.require_active()?;
                self.terminal = Some(Err(ProviderError::ProviderExecutionFailed));
                self.state = StreamState::Terminal;
                Ok(None)
            }
            "response.incomplete" => {
                self.require_active()?;
                let event: LifecycleEvent =
                    serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
                self.validate_matching_lifecycle_response(&event.response, &["incomplete"])?;
                self.terminal = Some(Err(ProviderError::IncompleteResponse));
                self.state = StreamState::Terminal;
                Ok(None)
            }
            "response.output_item.added"
            | "response.output_item.done"
            | "response.content_part.added"
            | "response.content_part.done"
            | "response.output_text.done"
            | "response.refusal.done"
            | "response.function_call_arguments.done"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_summary_text.done"
            | "response.reasoning_text.delta"
            | "response.reasoning_text.done" => {
                self.require_active()?;
                Ok(None)
            }
            _ => Err(ProviderError::MalformedResponse),
        }
    }

    fn require_active(&self) -> Result<(), ProviderError> {
        if self.state == StreamState::Active {
            Ok(())
        } else {
            Err(ProviderError::MalformedResponse)
        }
    }

    fn validate_lifecycle_response(
        &self,
        response: &LifecycleResponse,
        allowed_statuses: &[&str],
    ) -> Result<(), ProviderError> {
        validate_response_identifier(&response.id, MAX_PROVIDER_IDENTIFIER_BYTES)?;
        if response.object != "response"
            || !allowed_statuses.contains(&response.status.as_str())
            || response
                .model
                .as_deref()
                .is_some_and(|model| model != self.expected_model)
        {
            return Err(ProviderError::MalformedResponse);
        }
        Ok(())
    }

    fn validate_matching_lifecycle_response(
        &self,
        response: &LifecycleResponse,
        allowed_statuses: &[&str],
    ) -> Result<(), ProviderError> {
        self.validate_lifecycle_response(response, allowed_statuses)?;
        if self.response_id.as_deref() != Some(response.id.as_str()) {
            return Err(ProviderError::MalformedResponse);
        }
        Ok(())
    }

    fn record_text_delta(
        &mut self,
        sequence: u64,
        output_index: u32,
        content_index: u32,
        item_id: String,
        delta: String,
        refusal: bool,
    ) -> Result<ProviderStreamEvent, ProviderError> {
        validate_response_identifier(&item_id, MAX_PROVIDER_IDENTIFIER_BYTES)?;
        if delta.len() > MAX_DELTA_BYTES
            || output_index as usize >= MAX_OUTPUT_ITEMS
            || content_index as usize >= MAX_MESSAGE_CONTENT_PARTS
        {
            return Err(ProviderError::ResponseLimitExceeded);
        }
        let accumulator = self
            .text_deltas
            .entry((output_index, content_index))
            .or_insert_with(|| DeltaAccumulator {
                item_id: item_id.clone(),
                value: String::new(),
                refusal,
            });
        if accumulator.item_id != item_id || accumulator.refusal != refusal {
            return Err(ProviderError::MalformedResponse);
        }
        accumulator.value = append_bounded(
            std::mem::take(&mut accumulator.value),
            &delta,
            MAX_ACCUMULATED_TEXT_BYTES,
        )?;
        Ok(ProviderStreamEvent::TextDelta {
            provider_sequence: sequence,
            output_index,
            content_index,
            delta,
            refusal,
        })
    }

    fn record_argument_delta(
        &mut self,
        output_index: u32,
        item_id: String,
        delta: String,
    ) -> Result<(), ProviderError> {
        validate_response_identifier(&item_id, MAX_PROVIDER_IDENTIFIER_BYTES)?;
        if delta.len() > MAX_DELTA_BYTES || output_index as usize >= MAX_OUTPUT_ITEMS {
            return Err(ProviderError::ResponseLimitExceeded);
        }
        let accumulator =
            self.argument_deltas
                .entry(output_index)
                .or_insert_with(|| DeltaAccumulator {
                    item_id: item_id.clone(),
                    value: String::new(),
                    refusal: false,
                });
        if accumulator.item_id != item_id {
            return Err(ProviderError::MalformedResponse);
        }
        accumulator.value = append_bounded(
            std::mem::take(&mut accumulator.value),
            &delta,
            MAX_TOOL_ARGUMENT_BYTES,
        )?;
        Ok(())
    }

    fn parse_completed(
        &mut self,
        response: CompletedResponse,
    ) -> Result<ProviderOutcome, ProviderError> {
        #[cfg(test)]
        {
            self.diagnostic_stage = "validating completed-response identity";
        }
        validate_response_identifier(&response.id, MAX_PROVIDER_IDENTIFIER_BYTES)?;
        if self.response_id.as_deref() != Some(response.id.as_str())
            || response.object != "response"
            || response.model != self.expected_model
            || response.status != "completed"
            || response.output.is_empty()
            || response.output.len() > MAX_OUTPUT_ITEMS
        {
            return Err(ProviderError::MalformedResponse);
        }
        #[cfg(test)]
        {
            self.diagnostic_stage = "validating completed-response usage";
        }
        let usage = validate_usage(
            response.usage.ok_or(ProviderError::MalformedResponse)?,
            self.maximum_input_tokens,
            self.maximum_output_tokens,
        )?;
        let mut output = Vec::with_capacity(response.output.len());
        let mut item_ids = BTreeSet::new();
        let mut call_ids = BTreeSet::new();
        let mut total_text_bytes = 0_usize;
        for (output_index, item) in response.output.into_iter().enumerate() {
            let item_type = item
                .get("type")
                .and_then(Value::as_str)
                .ok_or(ProviderError::MalformedResponse)?;
            match item_type {
                "message" => {
                    #[cfg(test)]
                    {
                        self.diagnostic_stage = "validating a completed assistant message";
                    }
                    let message: WireOutputMessage = serde_json::from_value(item)
                        .map_err(|_| ProviderError::MalformedResponse)?;
                    validate_response_identifier(&message.id, MAX_PROVIDER_IDENTIFIER_BYTES)?;
                    if !item_ids.insert(message.id.clone())
                        || message.role != "assistant"
                        || message.status != "completed"
                        || message.content.is_empty()
                        || message.content.len() > MAX_MESSAGE_CONTENT_PARTS
                    {
                        return Err(ProviderError::MalformedResponse);
                    }
                    let mut text = String::new();
                    let mut refusal = false;
                    for (content_index, content) in message.content.into_iter().enumerate() {
                        let (part_text, part_refusal) = parse_message_content(content)?;
                        if !text.is_empty() && refusal != part_refusal {
                            return Err(ProviderError::MalformedResponse);
                        }
                        refusal = part_refusal;
                        self.validate_terminal_text(
                            output_index as u32,
                            content_index as u32,
                            &message.id,
                            &part_text,
                            part_refusal,
                        )?;
                        text = append_bounded(text, &part_text, MAX_ACCUMULATED_TEXT_BYTES)?;
                    }
                    total_text_bytes = total_text_bytes
                        .checked_add(text.len())
                        .filter(|bytes| *bytes <= MAX_ACCUMULATED_TEXT_BYTES)
                        .ok_or(ProviderError::ResponseLimitExceeded)?;
                    output.push(ProviderOutputItem::AssistantMessage(
                        ProviderAssistantMessage {
                            provider_item_id: message.id,
                            phase: message.phase,
                            text,
                            refusal,
                        },
                    ));
                }
                "reasoning" => {
                    #[cfg(test)]
                    {
                        self.diagnostic_stage = "validating a completed reasoning item";
                    }
                    let reasoning: WireReasoning = serde_json::from_value(item)
                        .map_err(|_| ProviderError::MalformedResponse)?;
                    validate_response_identifier(&reasoning.id, MAX_PROVIDER_IDENTIFIER_BYTES)?;
                    if !item_ids.insert(reasoning.id.clone())
                        || reasoning.status.as_deref() != Some("completed")
                        || reasoning.summary.len() > MAX_REASONING_SUMMARIES
                        || reasoning.content.is_some_and(|content| !content.is_empty())
                        || reasoning.encrypted_content.as_ref().is_some_and(|content| {
                            content.is_empty() || content.len() > MAX_ENCRYPTED_REASONING_BYTES
                        })
                    {
                        return Err(ProviderError::MalformedResponse);
                    }
                    let mut summaries = Vec::with_capacity(reasoning.summary.len());
                    for summary in reasoning.summary {
                        if summary.summary_type != "summary_text"
                            || summary.text.len() > MAX_ACCUMULATED_TEXT_BYTES
                        {
                            return Err(ProviderError::MalformedResponse);
                        }
                        total_text_bytes = total_text_bytes
                            .checked_add(summary.text.len())
                            .filter(|bytes| *bytes <= MAX_ACCUMULATED_TEXT_BYTES)
                            .ok_or(ProviderError::ResponseLimitExceeded)?;
                        summaries.push(summary.text);
                    }
                    output.push(ProviderOutputItem::Reasoning(ProviderReasoning {
                        provider_item_id: reasoning.id,
                        summaries,
                        encrypted_content: reasoning.encrypted_content,
                    }));
                }
                "function_call" => {
                    #[cfg(test)]
                    {
                        self.diagnostic_stage = "validating a completed function call";
                    }
                    let tool_call: WireFunctionCall = serde_json::from_value(item)
                        .map_err(|_| ProviderError::MalformedResponse)?;
                    validate_response_identifier(&tool_call.call_id, MAX_PROVIDER_CALL_ID_BYTES)?;
                    validate_response_tool_name(&tool_call.name)?;
                    if tool_call.status.as_deref() != Some("completed")
                        || !call_ids.insert(tool_call.call_id.clone())
                        || output
                            .iter()
                            .filter(|item| matches!(item, ProviderOutputItem::ToolCall(_)))
                            .count()
                            >= MAX_TOOL_COUNT
                        || tool_call.arguments.is_empty()
                        || tool_call.arguments.len() > MAX_TOOL_ARGUMENT_BYTES
                    {
                        return Err(ProviderError::MalformedResponse);
                    }
                    let arguments = parse_strict_value(tool_call.arguments.as_bytes())
                        .map_err(|_| ProviderError::MalformedResponse)?;
                    if !arguments.is_object() {
                        return Err(ProviderError::MalformedResponse);
                    }
                    if let Some(item_id) = tool_call.id.as_deref() {
                        validate_response_identifier(item_id, MAX_PROVIDER_IDENTIFIER_BYTES)?;
                        if !item_ids.insert(item_id.to_owned()) {
                            return Err(ProviderError::MalformedResponse);
                        }
                    }
                    self.validate_terminal_arguments(
                        output_index as u32,
                        tool_call.id.as_deref(),
                        &tool_call.arguments,
                    )?;
                    output.push(ProviderOutputItem::ToolCall(ProviderToolCall {
                        provider_item_id: tool_call.id,
                        provider_call_id: tool_call.call_id,
                        name: tool_call.name,
                        arguments: tool_call.arguments,
                    }));
                }
                _ => return Err(ProviderError::MalformedResponse),
            }
        }
        if !self.text_deltas.is_empty() || !self.argument_deltas.is_empty() {
            return Err(ProviderError::MalformedResponse);
        }
        Ok(ProviderOutcome {
            provider_response_id: response.id,
            output,
            usage,
        })
    }

    fn validate_terminal_text(
        &mut self,
        output_index: u32,
        content_index: u32,
        item_id: &str,
        text: &str,
        refusal: bool,
    ) -> Result<(), ProviderError> {
        let Some(delta) = self.text_deltas.remove(&(output_index, content_index)) else {
            return Ok(());
        };
        if delta.item_id != item_id || delta.value != text || delta.refusal != refusal {
            return Err(ProviderError::MalformedResponse);
        }
        Ok(())
    }

    fn validate_terminal_arguments(
        &mut self,
        output_index: u32,
        item_id: Option<&str>,
        arguments: &str,
    ) -> Result<(), ProviderError> {
        let Some(delta) = self.argument_deltas.remove(&output_index) else {
            return Ok(());
        };
        if item_id != Some(delta.item_id.as_str()) || delta.value != arguments {
            return Err(ProviderError::MalformedResponse);
        }
        Ok(())
    }
}

fn validate_ping_record(data: &[u8]) -> Result<(), ProviderError> {
    let value = parse_strict_value(data).map_err(|_| ProviderError::MalformedResponse)?;
    let mut event_nodes = 0;
    validate_event_value(&value, 0, &mut event_nodes)?;
    let object = value.as_object().ok_or(ProviderError::MalformedResponse)?;
    if object.len() > 2
        || object.get("type").and_then(Value::as_str) != Some("ping")
        || object
            .keys()
            .any(|key| !matches!(key.as_str(), "type" | "cost"))
    {
        return Err(ProviderError::MalformedResponse);
    }
    if let Some(cost) = object.get("cost") {
        let valid_string = cost.as_str().is_some_and(|cost| {
            !cost.is_empty()
                && cost.len() <= 64
                && cost.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        });
        if !valid_string && !cost.is_number() {
            return Err(ProviderError::MalformedResponse);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
mod validation;
mod wire;

use validation::*;
use wire::*;
