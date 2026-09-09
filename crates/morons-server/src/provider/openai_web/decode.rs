use super::{
    MAX_ANSWER_BYTES, MAX_ITEMS, MAX_RESPONSE_BYTES, MODEL, SearchResult,
    output::{self, array, number, string},
};
use crate::provider::{
    ProviderError,
    json::parse_strict_value,
    responses::{
        decode_usage, validate_event_value, validate_ping_record, validate_response_identifier,
    },
    sse::SseDecoder,
};
use crate::web_diagnostic::WebStage;
use serde_json::Value;
use std::collections::BTreeMap;

/// Decode a complete bounded response; never returns partial/delta-only answers.
pub fn decode_response(body: &[u8]) -> Result<SearchResult, ProviderError> {
    decode_response_at(body, &mut WebStage::BodyBounds)
}

pub(super) fn decode_response_at(
    body: &[u8],
    stage: &mut WebStage,
) -> Result<SearchResult, ProviderError> {
    *stage = WebStage::BodyBounds;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(ProviderError::ResponseLimitExceeded);
    }
    *stage = WebStage::Sse;
    let mut sse = SseDecoder::new();
    let records = sse.push(body)?;
    sse.finish()?;
    let mut sequence = 0;
    let mut response_id = None;
    let mut terminal = None;
    let mut done = false;
    let mut items = BTreeMap::<usize, Value>::new();
    let mut deltas = BTreeMap::<(usize, usize), (String, String)>::new();
    let mut annotations = BTreeMap::<(usize, usize, usize), (String, Value)>::new();
    let mut nodes = 0;
    let mut delta_bytes = 0;
    for record in records {
        if record.event.as_deref() == Some("ping") {
            *stage = WebStage::Sse;
            validate_ping_record(&record.data)?;
            continue;
        }
        *stage = WebStage::Termination;
        if record.data == b"[DONE]" {
            if terminal.is_none() || done || record.event.is_some() {
                return Err(ProviderError::MalformedResponse);
            }
            done = true;
            continue;
        }
        if terminal.is_some() || done {
            return Err(ProviderError::MalformedResponse);
        }
        *stage = WebStage::Json;
        let value =
            parse_strict_value(&record.data).map_err(|_| ProviderError::MalformedResponse)?;
        *stage = WebStage::EventEnvelope;
        validate_event_value(&value, 0, &mut nodes)?;
        let kind = string(&value, "type")?;
        if record.event.as_deref().is_some_and(|name| name != kind) {
            return Err(ProviderError::MalformedResponse);
        }
        *stage = WebStage::Sequence;
        if number(&value, "sequence_number")? != sequence {
            return Err(ProviderError::MalformedResponse);
        }
        sequence += 1;
        *stage = WebStage::EventKind;
        match kind {
            "response.created" | "response.in_progress" => {
                *stage = WebStage::Lifecycle;
                let response = value
                    .get("response")
                    .ok_or(ProviderError::MalformedResponse)?;
                *stage = WebStage::ResponseIdentity;
                let id = string(response, "id")?;
                validate_response_identifier(id, 128)?;
                *stage = WebStage::Lifecycle;
                if string(response, "object")? != "response"
                    || string(response, "status")? != "in_progress"
                {
                    return Err(ProviderError::MalformedResponse);
                }
                *stage = WebStage::ResponseModel;
                if response
                    .get("model")
                    .is_some_and(|m| !m.is_null() && m.as_str() != Some(MODEL))
                {
                    return Err(ProviderError::MalformedResponse);
                }
                *stage = WebStage::ResponseIdentity;
                if response_id.as_ref().is_some_and(|old| old != id)
                    || (kind == "response.created" && response_id.is_some())
                {
                    return Err(ProviderError::MalformedResponse);
                }
                response_id = Some(id.to_owned());
            }
            "response.completed" => {
                *stage = WebStage::Completion;
                let response = value
                    .get("response")
                    .ok_or(ProviderError::MalformedResponse)?;
                *stage = WebStage::ResponseIdentity;
                let id = string(response, "id")?;
                validate_response_identifier(id, 128)?;
                if response_id.as_deref() != Some(id) {
                    return Err(ProviderError::MalformedResponse);
                }
                *stage = WebStage::Lifecycle;
                if string(response, "object")? != "response" {
                    return Err(ProviderError::MalformedResponse);
                }
                *stage = WebStage::ResponseModel;
                if string(response, "model")? != MODEL {
                    return Err(ProviderError::MalformedResponse);
                }
                *stage = WebStage::Lifecycle;
                if string(response, "status")? != "completed" {
                    return Err(ProviderError::MalformedResponse);
                }
                *stage = WebStage::OutputItem;
                let output = array(response, "output")?;
                if output.len() > MAX_ITEMS {
                    return Err(ProviderError::ResponseLimitExceeded);
                }
                *stage = WebStage::OutputConsistency;
                if items.keys().copied().ne(0..items.len()) {
                    return Err(ProviderError::MalformedResponse);
                }
                let cached: Vec<Value> = items.into_values().collect();
                if !cached.is_empty() && !output.is_empty() && cached != output {
                    return Err(ProviderError::MalformedResponse);
                }
                let output = if output.is_empty() {
                    cached.as_slice()
                } else {
                    output
                };
                for ((index, part), (item_id, delta)) in &deltas {
                    let item = output.get(*index).ok_or(ProviderError::MalformedResponse)?;
                    let content = array(item, "content")?
                        .get(*part)
                        .ok_or(ProviderError::MalformedResponse)?;
                    if string(item, "id")? != item_id
                        || string(content, "type")? != "output_text"
                        || string(content, "text")? != delta
                    {
                        return Err(ProviderError::MalformedResponse);
                    }
                }
                *stage = WebStage::Citation;
                for ((index, part, annotation), (item_id, value)) in &annotations {
                    let item = output.get(*index).ok_or(ProviderError::MalformedResponse)?;
                    let content = array(item, "content")?
                        .get(*part)
                        .ok_or(ProviderError::MalformedResponse)?;
                    if string(item, "id")? != item_id
                        || array(content, "annotations")?.get(*annotation) != Some(value)
                    {
                        return Err(ProviderError::MalformedResponse);
                    }
                }
                *stage = WebStage::Usage;
                let usage = decode_usage(
                    response
                        .get("usage")
                        .cloned()
                        .ok_or(ProviderError::MalformedResponse)?,
                    96_000,
                    32_000,
                )?;
                terminal = Some(output::parse(output, usage, stage)?);
                items = BTreeMap::new();
            }
            "response.output_item.done" => {
                *stage = WebStage::Lifecycle;
                active(&response_id)?;
                *stage = WebStage::OutputItem;
                let index = index(&value)?;
                let item = value.get("item").ok_or(ProviderError::MalformedResponse)?;
                if !item.is_object() || items.insert(index, item.clone()).is_some() {
                    return Err(ProviderError::MalformedResponse);
                }
            }
            "response.output_text.delta" => {
                *stage = WebStage::Lifecycle;
                active(&response_id)?;
                *stage = WebStage::OutputConsistency;
                let index = index(&value)?;
                let part = usize::try_from(number(&value, "content_index")?)
                    .map_err(|_| ProviderError::MalformedResponse)?;
                if part >= 64 || items.contains_key(&index) {
                    return Err(ProviderError::MalformedResponse);
                }
                let id = string(&value, "item_id")?;
                validate_response_identifier(id, 128)?;
                let delta = string(&value, "delta")?;
                delta_bytes += delta.len();
                if delta_bytes > MAX_ANSWER_BYTES {
                    return Err(ProviderError::ResponseLimitExceeded);
                }
                let entry = deltas
                    .entry((index, part))
                    .or_insert_with(|| (id.into(), String::new()));
                if entry.0 != id {
                    return Err(ProviderError::MalformedResponse);
                }
                entry.1.push_str(delta);
            }
            "response.output_text.annotation.added" => {
                *stage = WebStage::Lifecycle;
                active(&response_id)?;
                *stage = WebStage::Citation;
                let index = index(&value)?;
                let part = usize::try_from(number(&value, "content_index")?)
                    .map_err(|_| ProviderError::MalformedResponse)?;
                let position = usize::try_from(number(&value, "annotation_index")?)
                    .map_err(|_| ProviderError::MalformedResponse)?;
                let id = string(&value, "item_id")?;
                validate_response_identifier(id, 128)?;
                if part >= 64 || position >= super::MAX_CITATIONS || items.contains_key(&index) {
                    return Err(ProviderError::MalformedResponse);
                }
                if let Some(annotation) = value.get("annotation").filter(|value| !value.is_null())
                    && (string(annotation, "type")? != "url_citation"
                        || annotations.len() >= super::MAX_CITATIONS
                        || annotations
                            .insert((index, part, position), (id.into(), annotation.clone()))
                            .is_some())
                {
                    return Err(ProviderError::MalformedResponse);
                }
            }
            "response.output_item.added" => {
                *stage = WebStage::Lifecycle;
                active(&response_id)?;
                *stage = WebStage::OutputItem;
                let position = index(&value)?;
                let item = value.get("item").ok_or(ProviderError::MalformedResponse)?;
                if items.contains_key(&position)
                    || !matches!(
                        string(item, "type")?,
                        "message" | "reasoning" | "web_search_call"
                    )
                {
                    return Err(ProviderError::MalformedResponse);
                }
            }
            "response.content_part.added"
            | "response.content_part.done"
            | "response.output_text.done"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_summary_text.done"
            | "response.web_search_call.in_progress"
            | "response.web_search_call.searching"
            | "response.web_search_call.completed" => {
                // Bounded progress grants no completed item, search or citation authority.
                *stage = WebStage::Lifecycle;
                active(&response_id)?;
            }
            _ => return Err(ProviderError::MalformedResponse),
        }
    }
    *stage = WebStage::Termination;
    terminal.ok_or(ProviderError::IncompleteResponse)
}
fn active(id: &Option<String>) -> Result<(), ProviderError> {
    id.as_ref()
        .map(|_| ())
        .ok_or(ProviderError::MalformedResponse)
}
fn index(value: &Value) -> Result<usize, ProviderError> {
    usize::try_from(number(value, "output_index")?)
        .ok()
        .filter(|index| *index < MAX_ITEMS)
        .ok_or(ProviderError::MalformedResponse)
}
