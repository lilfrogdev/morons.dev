use super::*;

pub(super) fn append_bounded(
    mut current: String,
    next: &str,
    maximum_bytes: usize,
) -> Result<String, ProviderError> {
    current
        .len()
        .checked_add(next.len())
        .filter(|length| *length <= maximum_bytes)
        .ok_or(ProviderError::ResponseLimitExceeded)?;
    current.push_str(next);
    Ok(current)
}

pub(super) fn parse_message_content(content: Value) -> Result<(String, bool), ProviderError> {
    let content_type = content
        .get("type")
        .and_then(Value::as_str)
        .ok_or(ProviderError::MalformedResponse)?;
    match content_type {
        "output_text" => {
            let content: WireOutputText =
                serde_json::from_value(content).map_err(|_| ProviderError::MalformedResponse)?;
            if !content.annotations.is_empty()
                || content
                    .logprobs
                    .is_some_and(|logprobs| !logprobs.is_empty())
                || content.text.len() > MAX_ACCUMULATED_TEXT_BYTES
            {
                return Err(ProviderError::MalformedResponse);
            }
            Ok((content.text, false))
        }
        "refusal" => {
            let content: WireRefusal =
                serde_json::from_value(content).map_err(|_| ProviderError::MalformedResponse)?;
            if content.refusal.len() > MAX_ACCUMULATED_TEXT_BYTES {
                return Err(ProviderError::ResponseLimitExceeded);
            }
            Ok((content.refusal, true))
        }
        _ => Err(ProviderError::MalformedResponse),
    }
}

pub(super) fn validate_usage(
    usage: WireUsage,
    maximum_input_tokens: u32,
    maximum_output_tokens: u32,
) -> Result<ProviderUsage, ProviderError> {
    let cached = usage.input_tokens_details.cached_tokens;
    let cache_write = usage.input_tokens_details.cache_write_tokens;
    let reasoning = usage.output_tokens_details.reasoning_tokens;
    if usage.input_tokens > u64::from(maximum_input_tokens)
        || usage.output_tokens > u64::from(maximum_output_tokens)
        || usage.total_tokens > MAX_USAGE_TOKENS
        || cached > usage.input_tokens
        || cache_write > usage.input_tokens
        || reasoning > usage.output_tokens
        || usage.input_tokens.checked_add(usage.output_tokens) != Some(usage.total_tokens)
    {
        return Err(ProviderError::MalformedResponse);
    }
    Ok(ProviderUsage {
        input_tokens: usage.input_tokens,
        cached_input_tokens: cached,
        cache_write_input_tokens: cache_write,
        output_tokens: usage.output_tokens,
        reasoning_output_tokens: reasoning,
        total_tokens: usage.total_tokens,
    })
}

pub(super) fn validate_response_identifier(
    value: &str,
    maximum_bytes: usize,
) -> Result<(), ProviderError> {
    validate_request_identifier(value, maximum_bytes).map_err(|_| ProviderError::MalformedResponse)
}

pub(super) fn validate_response_tool_name(name: &str) -> Result<(), ProviderError> {
    validate_request_tool_name(name).map_err(|_| ProviderError::MalformedResponse)
}

pub(super) fn validate_event_type(event_type: &str) -> Result<(), ProviderError> {
    if event_type.is_empty()
        || event_type.len() > 128
        || event_type
            .bytes()
            .any(|byte| !(0x21..=0x7e).contains(&byte))
    {
        return Err(ProviderError::MalformedResponse);
    }
    Ok(())
}

pub(super) fn validate_event_value(
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
