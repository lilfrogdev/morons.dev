use super::{
    DataUseRestrictions, ProviderCancellation, ProviderError,
    http_client::{ProviderHttpClient, bounded_client},
    json::parse_strict_value,
    response_http::{
        MAX_RESPONSE_HEADER_BYTES, MAX_RESPONSE_HEADERS, RESPONSE_HEADER_TIMEOUT, classify_status,
        next_frame, require_content_type, validate_content_length, validate_response_headers,
    },
    sse::SseDecoder,
};
use crate::tools::{ExaWebResult, WebSearchResult};
use http::{
    Request, Uri,
    header::{ACCEPT, CONTENT_TYPE},
};
use http_body_util::Full;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::{Instant, timeout_at};

const ENDPOINT: &str = "https://mcp.exa.ai/mcp?tools=web_search_exa";
pub(crate) const MAX_RESPONSE_BYTES: usize = 256 * 1024;

pub(crate) fn permits(policy: DataUseRestrictions) -> bool {
    !policy.block_training_use && !policy.require_zero_retention
}

#[derive(Debug, PartialEq)]
pub(crate) enum ExaFailure {
    BeforeDispatch(ProviderError),
    Uncertain(ProviderError),
}

pub(crate) struct ExaProvider {
    client: ProviderHttpClient,
    endpoint: Uri,
    timeout: Duration,
}
impl ExaProvider {
    pub(crate) fn new() -> Self {
        Self {
            client: bounded_client(
                false,
                Some((MAX_RESPONSE_HEADERS, MAX_RESPONSE_HEADER_BYTES)),
            ),
            endpoint: ENDPOINT.parse().expect("reviewed Exa route"),
            timeout: Duration::from_secs(60),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(endpoint: Uri) -> Self {
        Self {
            endpoint,
            client: bounded_client(
                true,
                Some((MAX_RESPONSE_HEADERS, MAX_RESPONSE_HEADER_BYTES)),
            ),
            ..Self::new()
        }
    }

    // Durable admission is required even when cancellation subsequently prevents dispatch.
    pub(crate) async fn execute(
        &self,
        query: &str,
        cancellation: &mut ProviderCancellation,
    ) -> Result<ExaWebResult, ExaFailure> {
        if cancellation.is_cancelled() {
            return Err(ExaFailure::BeforeDispatch(ProviderError::Cancelled));
        }
        let body = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
            "name":"web_search_exa","arguments":{"query":query,"numResults":5}
        }})
        .to_string();
        let request = Request::builder()
            .method("POST")
            .uri(self.endpoint.clone())
            .header(ACCEPT, "application/json, text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .body(Full::new(bytes::Bytes::from(body)))
            .map_err(|_| ExaFailure::BeforeDispatch(ProviderError::InvalidRequest))?;
        self.send(query, request, cancellation).await
    }

    async fn send(
        &self,
        query: &str,
        request: Request<Full<bytes::Bytes>>,
        cancellation: &mut ProviderCancellation,
    ) -> Result<ExaWebResult, ExaFailure> {
        let deadline = Instant::now() + self.timeout;
        let headers = (Instant::now() + RESPONSE_HEADER_TIMEOUT).min(deadline);
        if cancellation.is_cancelled() {
            return Err(ExaFailure::BeforeDispatch(ProviderError::Cancelled));
        }
        // Once the request future can be polled, cancellation cannot prove absence of effects.
        let response = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(ExaFailure::Uncertain(ProviderError::Cancelled)),
            result = timeout_at(headers, self.client.request(request)) => result
                .map_err(|_| if headers == deadline { ProviderError::TotalTimeout } else { ProviderError::ResponseHeaderTimeout })
                .and_then(|response| response.map_err(|_| ProviderError::Transport))
                .map_err(ExaFailure::Uncertain)?,
        };
        self.receive(query, response, deadline, cancellation)
            .await
            .map_err(ExaFailure::Uncertain)
    }

    async fn receive(
        &self,
        query: &str,
        response: http::Response<hyper::body::Incoming>,
        deadline: Instant,
        cancellation: &mut ProviderCancellation,
    ) -> Result<ExaWebResult, ProviderError> {
        validate_response_headers(response.headers())?;
        if response.status() != http::StatusCode::OK {
            return Err(classify_status(response.status()));
        }
        for name in [CONTENT_TYPE, http::header::CONTENT_LENGTH] {
            if response.headers().get_all(name).iter().nth(1).is_some() {
                return Err(ProviderError::MalformedResponse);
            }
        }
        let sse = if require_content_type(response.headers(), "application/json").is_ok() {
            false
        } else {
            require_content_type(response.headers(), "text/event-stream")?;
            true
        };
        validate_content_length(response.headers(), MAX_RESPONSE_BYTES)?;
        let mut incoming = response.into_body();
        let mut body = Vec::new();
        while let Some(frame) = next_frame(&mut incoming, deadline, cancellation).await? {
            let data = frame
                .into_data()
                .map_err(|_| ProviderError::MalformedResponse)?;
            if data.len() > MAX_RESPONSE_BYTES.saturating_sub(body.len()) {
                return Err(ProviderError::ResponseLimitExceeded);
            }
            body.extend_from_slice(&data);
        }
        if cancellation.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        decode(query, &body, sse)
    }
}

fn decode(query: &str, body: &[u8], sse: bool) -> Result<ExaWebResult, ProviderError> {
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(ProviderError::ResponseLimitExceeded);
    }
    let payload;
    let bytes = if sse {
        let mut decoder = SseDecoder::new();
        let mut records = decoder.push(body)?;
        decoder.finish()?;
        if records.len() != 1 {
            return Err(ProviderError::MalformedResponse);
        }
        let record = records.pop().ok_or(ProviderError::MalformedResponse)?;
        if record
            .event
            .as_deref()
            .is_some_and(|event| event != "message")
        {
            return Err(ProviderError::MalformedResponse);
        }
        payload = record.data;
        &payload
    } else {
        body
    };
    let rpc = parse_strict_value(bytes).map_err(|_| ProviderError::MalformedResponse)?;
    if rpc.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || rpc.get("id").and_then(Value::as_u64) != Some(1)
        || rpc.get("error").is_some()
    {
        return Err(ProviderError::MalformedResponse);
    }
    let result = rpc.get("result").ok_or(ProviderError::MalformedResponse)?;
    if result
        .get("isError")
        .is_some_and(|v| v.as_bool() != Some(false))
    {
        return Err(ProviderError::ProviderExecutionFailed);
    }
    let mut results = Vec::new();
    if let Some(structured) = result.get("structuredContent") {
        results = structured_results(structured)?;
    } else {
        let content = result
            .get("content")
            .and_then(Value::as_array)
            .ok_or(ProviderError::MalformedResponse)?;
        if content.is_empty() || content.len() > 10 {
            return Err(ProviderError::MalformedResponse);
        }
        for item in content {
            if item.get("type").and_then(Value::as_str) != Some("text") {
                return Err(ProviderError::MalformedResponse);
            }
            let text = item
                .get("text")
                .and_then(Value::as_str)
                .ok_or(ProviderError::MalformedResponse)?;
            if text.trim_start().starts_with('{') {
                results.extend(structured_results(
                    &parse_strict_value(text.as_bytes())
                        .map_err(|_| ProviderError::MalformedResponse)?,
                )?);
            } else {
                results.extend(text_results(text)?);
            }
            if results.len() > 10 {
                return Err(ProviderError::ResponseLimitExceeded);
            }
        }
    }
    let result = ExaWebResult {
        query: query.to_owned(),
        contract_revision: 1,
        results,
    };
    if !result.is_valid() {
        return Err(ProviderError::MalformedResponse);
    }
    Ok(result)
}

fn structured_results(value: &Value) -> Result<Vec<WebSearchResult>, ProviderError> {
    let items = value
        .get("results")
        .and_then(Value::as_array)
        .ok_or(ProviderError::MalformedResponse)?;
    if items.is_empty() || items.len() > 10 {
        return Err(ProviderError::MalformedResponse);
    }
    items
        .iter()
        .map(|item| {
            let field = |key| {
                item.get(key)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or(ProviderError::MalformedResponse)
            };
            Ok(WebSearchResult {
                title: field("title")?,
                url: field("url")?,
                snippet: excerpt(&field("text")?),
            })
        })
        .collect()
}

fn text_results(text: &str) -> Result<Vec<WebSearchResult>, ProviderError> {
    let text = text.trim();
    if !text.starts_with("Title: ") {
        return Err(ProviderError::MalformedResponse);
    }
    let mut results = Vec::new();
    for block in text[7..].split("\nTitle: ") {
        let (title, rest) = block
            .split_once('\n')
            .ok_or(ProviderError::MalformedResponse)?;
        let (metadata, snippet) = rest
            .split_once("\nText: ")
            .or_else(|| rest.split_once("\nHighlights:\n"))
            .ok_or(ProviderError::MalformedResponse)?;
        let mut urls = metadata
            .lines()
            .filter_map(|line| line.strip_prefix("URL: "));
        let url = urls.next().ok_or(ProviderError::MalformedResponse)?;
        if urls.next().is_some() {
            return Err(ProviderError::MalformedResponse);
        }
        results.push(WebSearchResult {
            title: title.trim().to_owned(),
            url: url.trim().to_owned(),
            snippet: excerpt(snippet.trim_end_matches("\n---")),
        });
        if results.len() > 10 {
            return Err(ProviderError::ResponseLimitExceeded);
        }
    }
    Ok(results)
}

fn excerpt(text: &str) -> String {
    let text = text.trim();
    let mut end = text.len().min(crate::tools::MAX_WEB_SEARCH_SNIPPET_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests;
