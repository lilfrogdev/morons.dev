use super::{Citation, MAX_ACTIONS, MAX_ANSWER_BYTES, MAX_CITATIONS, MAX_ITEMS, SearchResult};
use crate::provider::{ProviderError, ProviderUsage, responses::validate_response_identifier};
use http::Uri;
use serde_json::Value;
use std::collections::BTreeSet;

pub(super) fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, ProviderError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or(ProviderError::MalformedResponse)
}
pub(super) fn array<'a>(value: &'a Value, key: &str) -> Result<&'a [Value], ProviderError> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or(ProviderError::MalformedResponse)
}
pub(super) fn number(value: &Value, key: &str) -> Result<u64, ProviderError> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or(ProviderError::MalformedResponse)
}
pub(super) fn url(value: &str) -> Result<(), ProviderError> {
    if value.is_empty() || value.len() > 4096 || !value.bytes().all(|b| (0x21..=0x7e).contains(&b))
    {
        return Err(ProviderError::MalformedResponse);
    }
    let base = value
        .split('#')
        .next()
        .ok_or(ProviderError::MalformedResponse)?;
    let parsed: Uri = base.parse().map_err(|_| ProviderError::MalformedResponse)?;
    if !matches!(parsed.scheme_str(), Some("http" | "https"))
        || parsed.host().is_none_or(str::is_empty)
        || parsed
            .authority()
            .is_none_or(|a| a.as_str().contains(['@', '%', '\\']))
    {
        return Err(ProviderError::MalformedResponse);
    }
    Ok(())
}

pub(super) fn parse(items: &[Value], usage: ProviderUsage) -> Result<SearchResult, ProviderError> {
    if items.is_empty() || items.len() > MAX_ITEMS {
        return Err(ProviderError::MalformedResponse);
    }
    let mut ids = BTreeSet::new();
    let mut result = SearchResult {
        answer: String::new(),
        citations: Vec::new(),
        search_calls: 0,
        open_page_calls: 0,
        find_in_page_calls: 0,
        usage,
    };
    let mut final_messages = 0;
    for item in items {
        let id = string(item, "id")?;
        validate_response_identifier(id, 128)?;
        if !ids.insert(id) {
            return Err(ProviderError::MalformedResponse);
        }
        match string(item, "type")? {
            "web_search_call" => {
                if string(item, "status")? != "completed" {
                    return Err(ProviderError::MalformedResponse);
                }
                let action = item.get("action").ok_or(ProviderError::MalformedResponse)?;
                match string(action, "type")? {
                    "search" => {
                        // Search queries can be omitted by the provider, but malformed present fields reject.
                        if let Some(query) = action.get("query").filter(|v| !v.is_null()) {
                            bounded_query(query)?;
                        }
                        if let Some(queries) = action.get("queries").filter(|v| !v.is_null()) {
                            let queries =
                                queries.as_array().ok_or(ProviderError::MalformedResponse)?;
                            if queries.len() > 8 {
                                return Err(ProviderError::ResponseLimitExceeded);
                            }
                            for query in queries {
                                bounded_query(query)?;
                            }
                        }
                        if let Some(sources) = action.get("sources").filter(|v| !v.is_null()) {
                            let sources =
                                sources.as_array().ok_or(ProviderError::MalformedResponse)?;
                            if sources.len() > MAX_CITATIONS {
                                return Err(ProviderError::ResponseLimitExceeded);
                            }
                            for source in sources {
                                if string(source, "type")? != "url" {
                                    return Err(ProviderError::MalformedResponse);
                                }
                                url(string(source, "url")?)?;
                            }
                        }
                        result.search_calls += 1;
                    }
                    "open_page" => {
                        if let Some(source) = action.get("url").filter(|v| !v.is_null()) {
                            url(source.as_str().ok_or(ProviderError::MalformedResponse)?)?;
                        }
                        result.open_page_calls += 1;
                    }
                    "find_in_page" => {
                        url(string(action, "url")?)?;
                        bounded_query(
                            action
                                .get("pattern")
                                .ok_or(ProviderError::MalformedResponse)?,
                        )?;
                        result.find_in_page_calls += 1;
                    }
                    _ => return Err(ProviderError::MalformedResponse),
                }
                if usize::from(
                    result.search_calls + result.open_page_calls + result.find_in_page_calls,
                ) > MAX_ACTIONS
                {
                    return Err(ProviderError::ResponseLimitExceeded);
                }
            }
            "message" => {
                if string(item, "role")? != "assistant" || string(item, "status")? != "completed" {
                    return Err(ProviderError::MalformedResponse);
                }
                let phase = item.get("phase").and_then(Value::as_str);
                if item.get("phase").is_some_and(|p| !p.is_null())
                    && !matches!(phase, Some("commentary" | "final_answer"))
                {
                    return Err(ProviderError::MalformedResponse);
                }
                let content = array(item, "content")?;
                if content.is_empty() || content.len() > 64 {
                    return Err(ProviderError::MalformedResponse);
                }
                let mut answer = String::new();
                let mut citations = Vec::new();
                for part in content {
                    if string(part, "type")? != "output_text"
                        || part.get("logprobs").is_some_and(|v| {
                            !v.is_null() && v.as_array().is_none_or(|v| !v.is_empty())
                        })
                    {
                        return Err(ProviderError::MalformedResponse);
                    }
                    let text = string(part, "text")?;
                    if text.len() + answer.len() > MAX_ANSWER_BYTES {
                        return Err(ProviderError::ResponseLimitExceeded);
                    }
                    let annotations = array(part, "annotations")?;
                    if annotations.len() + citations.len() > MAX_CITATIONS {
                        return Err(ProviderError::ResponseLimitExceeded);
                    }
                    for annotation in annotations {
                        if string(annotation, "type")? != "url_citation" {
                            return Err(ProviderError::MalformedResponse);
                        }
                        let source = string(annotation, "url")?;
                        url(source)?;
                        let title = string(annotation, "title")?;
                        let start = number(annotation, "start_index")?;
                        let end = number(annotation, "end_index")?;
                        // Offsets remain provider metadata. No UTF-8 slicing or unit conversion.
                        if title.len() > 512 || start > end || end > text.len() as u64 {
                            return Err(ProviderError::MalformedResponse);
                        }
                        citations.push(Citation {
                            url: source.into(),
                            title: title.into(),
                        });
                    }
                    answer.push_str(text);
                }
                if phase != Some("commentary") {
                    final_messages += 1;
                    result.answer = answer;
                    result.citations = citations;
                }
            }
            "reasoning" => {
                // No reasoning content or summaries are promoted into the search result.
                if item
                    .get("status")
                    .is_some_and(|s| !s.is_null() && s.as_str() != Some("completed"))
                {
                    return Err(ProviderError::MalformedResponse);
                }
            }
            _ => return Err(ProviderError::MalformedResponse),
        }
    }
    if final_messages != 1
        || result.answer.trim().is_empty()
        || result.citations.is_empty()
        || result.search_calls == 0
    {
        return Err(ProviderError::MalformedResponse);
    }
    Ok(result)
}
fn bounded_query(value: &Value) -> Result<(), ProviderError> {
    let value = value.as_str().ok_or(ProviderError::MalformedResponse)?;
    if value.trim().is_empty() || value.len() > 4096 {
        return Err(ProviderError::MalformedResponse);
    }
    Ok(())
}
