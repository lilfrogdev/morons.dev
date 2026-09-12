use super::response_diagnostic::ResponseStage;
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
};

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

pub(super) struct ResponsesDecoder {
    sse: SseDecoder,
    expected_model: &'static str,
    response_model_alias: Option<&'static str>,
    maximum_input_tokens: u32,
    maximum_output_tokens: u32,
    expected_sequence: u64,
    state: StreamState,
    response_id: Option<String>,
    text_deltas: BTreeMap<(u32, u32), DeltaAccumulator>,
    argument_deltas: BTreeMap<u32, DeltaAccumulator>,
    terminal: Option<Result<ProviderOutcome, ProviderError>>,
    native_items: Option<native::NativeCompletedItems>,
    stage: Cell<ResponseStage>,
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
            response_model_alias: None,
            maximum_input_tokens,
            maximum_output_tokens,
            expected_sequence: 0,
            state: StreamState::AwaitingCreated,
            response_id: None,
            text_deltas: BTreeMap::new(),
            argument_deltas: BTreeMap::new(),
            terminal: None,
            native_items: None,
            stage: Cell::new(ResponseStage::SseFraming),
        }
    }

    pub(super) fn new_native(
        expected_model: &'static str,
        maximum_input_tokens: u32,
        maximum_output_tokens: u32,
    ) -> Self {
        let mut decoder = Self::new(expected_model, maximum_input_tokens, maximum_output_tokens);
        decoder.response_model_alias = native::response_model_alias(expected_model);
        decoder.native_items = Some(native::NativeCompletedItems::default());
        decoder
    }

    fn matches_response_model(&self, model: &str) -> bool {
        model == self.expected_model || self.response_model_alias == Some(model)
    }

    pub(super) fn failure_stage(&self) -> ResponseStage {
        self.stage.get()
    }

    pub(super) fn push(&mut self, chunk: &[u8]) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
        self.stage.set(ResponseStage::SseFraming);
        let records = self.sse.push(chunk)?;
        let mut events = Vec::new();
        for record in records {
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
        self.stage.set(ResponseStage::SseFraming);
        if record.event.as_deref() == Some("ping") {
            validate_ping_record(&record.data)?;
            return Ok(None);
        }
        self.stage.set(ResponseStage::Lifecycle);
        if record.data == b"[DONE]" {
            if record.event.is_some() || self.state != StreamState::Terminal {
                return Err(ProviderError::MalformedResponse);
            }
            self.state = StreamState::DoneMarker;
            return Ok(None);
        }
        if matches!(self.state, StreamState::Terminal | StreamState::DoneMarker) {
            return Err(ProviderError::MalformedResponse);
        }
        self.stage.set(ResponseStage::Json);
        let value =
            parse_strict_value(&record.data).map_err(|_| ProviderError::MalformedResponse)?;
        self.stage.set(ResponseStage::EventBounds);
        let mut event_nodes = 0;
        validate_event_value(&value, 0, &mut event_nodes)?;
        self.stage.set(
            if value
                .get("sequence_number")
                .and_then(Value::as_u64)
                .is_none()
            {
                ResponseStage::SequenceField
            } else {
                ResponseStage::EventEnvelope
            },
        );
        let envelope: EventEnvelope =
            serde_json::from_value(value.clone()).map_err(|_| ProviderError::MalformedResponse)?;
        self.stage.set(ResponseStage::EventKind);
        validate_event_type(&envelope.event_type)?;
        self.stage.set(ResponseStage::EventEnvelope);
        if record
            .event
            .as_deref()
            .is_some_and(|event| event != envelope.event_type)
        {
            return Err(ProviderError::MalformedResponse);
        }
        self.stage.set(ResponseStage::Sequence);
        if envelope.sequence_number != self.expected_sequence {
            return Err(ProviderError::MalformedResponse);
        }
        self.expected_sequence = self
            .expected_sequence
            .checked_add(1)
            .ok_or(ProviderError::ResponseLimitExceeded)?;

        self.stage.set(ResponseStage::Lifecycle);
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
                self.stage.set(ResponseStage::TextDelta);
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
                self.stage.set(ResponseStage::RefusalDelta);
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
                self.stage.set(ResponseStage::ArgumentDelta);
                let event: FunctionArgumentsDeltaEvent =
                    serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
                self.record_argument_delta(event.output_index, event.item_id, event.delta)?;
                Ok(None)
            }
            "response.completed" => {
                self.require_active()?;
                self.stage.set(ResponseStage::CompletedEnvelope);
                let event: CompletedEvent =
                    serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
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
            "response.output_item.done" if self.native_items.is_some() => {
                self.require_active()?;
                self.stage.set(ResponseStage::OutputConsistency);
                self.native_items
                    .as_mut()
                    .expect("native mode checked")
                    .push(value, record.data.len(), event_nodes)?;
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
            _ => {
                self.stage.set(ResponseStage::EventKind);
                Err(ProviderError::MalformedResponse)
            }
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
        self.stage.set(ResponseStage::ResponseIdentity);
        validate_response_identifier(&response.id, MAX_PROVIDER_IDENTIFIER_BYTES)?;
        if response.object != "response" || !allowed_statuses.contains(&response.status.as_str()) {
            return Err(ProviderError::MalformedResponse);
        }
        self.stage.set(ResponseStage::ResponseModel);
        if response
            .model
            .as_deref()
            .is_some_and(|model| !self.matches_response_model(model))
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
        self.stage.set(ResponseStage::ResponseIdentity);
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
        self.require_open_item(output_index)?;
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
        self.require_open_item(output_index)?;
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

    fn require_open_item(&self, index: u32) -> Result<(), ProviderError> {
        if self
            .native_items
            .as_ref()
            .is_some_and(|items| items.contains(index))
        {
            return Err(ProviderError::MalformedResponse);
        }
        Ok(())
    }

    fn parse_completed(
        &mut self,
        response: CompletedResponse,
    ) -> Result<ProviderOutcome, ProviderError> {
        self.stage.set(ResponseStage::CompletedIdentity);
        validate_response_identifier(&response.id, MAX_PROVIDER_IDENTIFIER_BYTES)?;
        self.stage.set(ResponseStage::ResponseModel);
        if !self.matches_response_model(&response.model) {
            return Err(ProviderError::MalformedResponse);
        }
        self.stage.set(ResponseStage::CompletedIdentity);
        if self.response_id.as_deref() != Some(response.id.as_str())
            || response.object != "response"
            || response.status != "completed"
            || (response.output.is_empty()
                && self
                    .native_items
                    .as_ref()
                    .is_none_or(|items| items.is_empty()))
            || response.output.len() > MAX_OUTPUT_ITEMS
        {
            return Err(ProviderError::MalformedResponse);
        }
        self.stage.set(ResponseStage::Usage);
        let usage = validate_usage(
            serde_json::from_value(response.usage.ok_or(ProviderError::MalformedResponse)?)
                .map_err(|_| ProviderError::MalformedResponse)?,
            self.maximum_input_tokens,
            self.maximum_output_tokens,
        )?;
        self.stage.set(ResponseStage::OutputConsistency);
        let items = self
            .native_items
            .take()
            .map(native::NativeCompletedItems::finish)
            .transpose()?
            .flatten();
        let output = if let Some(items) = items {
            let output = self.parse_output(items)?;
            if !response.output.is_empty() {
                let terminal = self.parse_output(response.output)?;
                self.stage.set(ResponseStage::OutputConsistency);
                if terminal != output {
                    return Err(ProviderError::MalformedResponse);
                }
            }
            output
        } else {
            self.parse_output(response.output)?
        };
        Ok(ProviderOutcome {
            provider_response_id: response.id,
            output,
            usage,
        })
    }

    fn parse_output(
        &mut self,
        items: Vec<Value>,
    ) -> Result<Vec<ProviderOutputItem>, ProviderError> {
        let mut output = Vec::with_capacity(items.len());
        let mut item_ids = BTreeSet::new();
        let mut call_ids = BTreeSet::new();
        let mut total_text_bytes = 0_usize;
        for (output_index, item) in items.into_iter().enumerate() {
            self.stage.set(ResponseStage::OutputConsistency);
            let item_type = item
                .get("type")
                .and_then(Value::as_str)
                .ok_or(ProviderError::MalformedResponse)?;
            match item_type {
                "message" => {
                    self.stage.set(ResponseStage::AssistantMessage);
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
                    self.stage.set(ResponseStage::Reasoning);
                    let reasoning: WireReasoning = serde_json::from_value(item)
                        .map_err(|_| ProviderError::MalformedResponse)?;
                    validate_response_identifier(&reasoning.id, MAX_PROVIDER_IDENTIFIER_BYTES)?;
                    if !item_ids.insert(reasoning.id.clone())
                        || reasoning
                            .status
                            .as_deref()
                            .is_some_and(|status| status != "completed")
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
                    self.stage.set(ResponseStage::FunctionCall);
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
                        opaque_continuation: None,
                    }));
                }
                _ => return Err(ProviderError::MalformedResponse),
            }
        }
        self.stage.set(ResponseStage::OutputConsistency);
        if !self.text_deltas.is_empty() || !self.argument_deltas.is_empty() {
            return Err(ProviderError::MalformedResponse);
        }
        Ok(output)
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

pub(super) fn validate_ping_record(data: &[u8]) -> Result<(), ProviderError> {
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
mod diagnostic_tests;
mod native;
#[cfg(test)]
pub(super) mod native_tests;
#[cfg(test)]
mod tests;
mod validation;
mod wire;

use validation::{
    append_bounded, parse_message_content, validate_event_type, validate_response_tool_name,
    validate_usage,
};
pub(super) use validation::{validate_event_value, validate_response_identifier};

pub(super) fn decode_usage(
    value: Value,
    input_limit: u32,
    output_limit: u32,
) -> Result<ProviderUsage, ProviderError> {
    let usage = serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
    validate_usage(usage, input_limit, output_limit)
}
use wire::*;
