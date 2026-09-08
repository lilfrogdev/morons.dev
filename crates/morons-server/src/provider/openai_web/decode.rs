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
use serde_json::Value;
use std::collections::BTreeMap;

/// Decode a complete bounded response; never returns partial/delta-only answers.
pub fn decode_response(body: &[u8]) -> Result<SearchResult, ProviderError> {
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(ProviderError::ResponseLimitExceeded);
    }
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
            validate_ping_record(&record.data)?;
            continue;
        }
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
        let value =
            parse_strict_value(&record.data).map_err(|_| ProviderError::MalformedResponse)?;
        validate_event_value(&value, 0, &mut nodes)?;
        let kind = string(&value, "type")?;
        if record.event.as_deref().is_some_and(|name| name != kind)
            || number(&value, "sequence_number")? != sequence
        {
            return Err(ProviderError::MalformedResponse);
        }
        sequence += 1;
        match kind {
            "response.created" | "response.in_progress" => {
                let response = value
                    .get("response")
                    .ok_or(ProviderError::MalformedResponse)?;
                let id = string(response, "id")?;
                validate_response_identifier(id, 128)?;
                if string(response, "object")? != "response"
                    || string(response, "status")? != "in_progress"
                    || response
                        .get("model")
                        .is_some_and(|m| !m.is_null() && m.as_str() != Some(MODEL))
                    || response_id.as_ref().is_some_and(|old| old != id)
                    || (kind == "response.created" && response_id.is_some())
                {
                    return Err(ProviderError::MalformedResponse);
                }
                response_id = Some(id.to_owned());
            }
            "response.completed" => {
                let response = value
                    .get("response")
                    .ok_or(ProviderError::MalformedResponse)?;
                let id = string(response, "id")?;
                validate_response_identifier(id, 128)?;
                if response_id.as_deref() != Some(id)
                    || string(response, "object")? != "response"
                    || string(response, "model")? != MODEL
                    || string(response, "status")? != "completed"
                {
                    return Err(ProviderError::MalformedResponse);
                }
                let output = array(response, "output")?;
                if output.len() > MAX_ITEMS {
                    return Err(ProviderError::ResponseLimitExceeded);
                }
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
                let usage = decode_usage(
                    response
                        .get("usage")
                        .cloned()
                        .ok_or(ProviderError::MalformedResponse)?,
                    96_000,
                    32_000,
                )?;
                terminal = Some(output::parse(output, usage)?);
                items = BTreeMap::new();
            }
            "response.output_item.done" => {
                active(&response_id)?;
                let index = index(&value)?;
                let item = value.get("item").ok_or(ProviderError::MalformedResponse)?;
                if !item.is_object() || items.insert(index, item.clone()).is_some() {
                    return Err(ProviderError::MalformedResponse);
                }
            }
            "response.output_text.delta" => {
                active(&response_id)?;
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
                active(&response_id)?;
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
                active(&response_id)?;
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
                active(&response_id)?;
            }
            _ => return Err(ProviderError::MalformedResponse),
        }
    }
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
