use std::{collections::BTreeSet, time::Duration};

use bytes::{Bytes, BytesMut};
use http::{
    HeaderValue, Method, Request, StatusCode, Uri,
    header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE, USER_AGENT},
};
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};
use serde::Deserialize;
use tokio::time::{self, Instant};
use zeroize::Zeroizing;

use super::{
    MAX_WEB_SEARCH_BODY_BYTES, MAX_WEB_SEARCH_RESULTS, MAX_WEB_SEARCH_SNIPPET_BYTES,
    MAX_WEB_SEARCH_TITLE_BYTES, MAX_WEB_SEARCH_URL_BYTES, ToolErrorKind, ToolInput, ToolOutput,
    ToolResult, WebSearchResult,
};
use crate::{provider::ProviderCancellation, provider::json::parse_strict_value};

const SEARCH_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";
const SEARCH_CREDENTIAL_ENVIRONMENT_VARIABLE: &str = "BRAVE_SEARCH_API_KEY";
const MAX_SEARCH_CREDENTIAL_BYTES: usize = 4 * 1024;
const USER_AGENT_VALUE: &str = concat!("morons-server/", env!("CARGO_PKG_VERSION"));
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HEADER_TIMEOUT: Duration = Duration::from_secs(15);
const INACTIVITY_TIMEOUT: Duration = Duration::from_secs(10);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESPONSE_HEADERS: usize = 64;
const MAX_RESPONSE_HEADER_BYTES: usize = 16 * 1024;

type SearchHttpClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

pub(crate) struct WebSearchToolExecutor {
    client: SearchHttpClient,
    endpoint: String,
    credential: Option<Zeroizing<Vec<u8>>>,
}

impl WebSearchToolExecutor {
    pub(crate) fn new() -> Self {
        let credential = std::env::var(SEARCH_CREDENTIAL_ENVIRONMENT_VARIABLE)
            .ok()
            .map(String::into_bytes)
            .filter(|value| !value.is_empty() && value.len() <= MAX_SEARCH_CREDENTIAL_BYTES)
            .map(Zeroizing::new);
        Self::build(SEARCH_ENDPOINT.to_owned(), credential, false)
    }

    #[cfg(test)]
    pub(crate) fn for_test(endpoint: String) -> Self {
        Self::build(
            endpoint,
            Some(Zeroizing::new(b"not-a-real-search-key".to_vec())),
            true,
        )
    }

    fn build(endpoint: String, credential: Option<Zeroizing<Vec<u8>>>, allow_http: bool) -> Self {
        let mut http = HttpConnector::new();
        http.enforce_http(false);
        http.set_connect_timeout(Some(CONNECT_TIMEOUT));
        http.set_nodelay(true);
        let tls = HttpsConnectorBuilder::new().with_webpki_roots();
        let tls = if allow_http {
            tls.https_or_http()
        } else {
            tls.https_only()
        };
        let connector = tls.enable_http1().wrap_connector(http);
        let mut builder = Client::builder(TokioExecutor::new());
        builder.retry_canceled_requests(false);
        builder.pool_idle_timeout(Duration::from_secs(30));
        builder.pool_max_idle_per_host(2);
        Self {
            client: builder.build(connector),
            endpoint,
            credential,
        }
    }

    pub(crate) async fn execute(
        &self,
        input: &ToolInput,
        cancellation: &ProviderCancellation,
    ) -> ToolResult {
        let ToolInput::WebSearch { query } = input else {
            return ToolResult::error(ToolErrorKind::InvalidResponse);
        };
        match self.search(query, cancellation.clone()).await {
            Ok(results) => ToolResult::Ok {
                output: ToolOutput::WebSearch {
                    query: query.clone(),
                    truncated: results.len() == MAX_WEB_SEARCH_RESULTS,
                    results,
                },
            },
            Err(error) => ToolResult::error(error),
        }
    }

    async fn search(
        &self,
        query: &str,
        mut cancellation: ProviderCancellation,
    ) -> Result<Vec<WebSearchResult>, ToolErrorKind> {
        if cancellation.is_cancelled() {
            return Err(ToolErrorKind::Cancelled);
        }
        let credential = self
            .credential
            .as_deref()
            .ok_or(ToolErrorKind::CredentialNotConfigured)?;
        let mut authorization = HeaderValue::from_bytes(credential)
            .map_err(|_| ToolErrorKind::CredentialNotConfigured)?;
        authorization.set_sensitive(true);
        let uri = search_uri(&self.endpoint, query)?;
        let request = Request::builder()
            .method(Method::GET)
            .uri(uri)
            .header(ACCEPT, "application/json")
            .header("x-subscription-token", authorization)
            .header(USER_AGENT, USER_AGENT_VALUE)
            .body(Full::new(Bytes::new()))
            .map_err(|_| ToolErrorKind::Network)?;
        let deadline = Instant::now() + TOTAL_TIMEOUT;
        let response = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(ToolErrorKind::Cancelled),
            response = time::timeout(HEADER_TIMEOUT, self.client.request(request)) => {
                response.map_err(|_| ToolErrorKind::TimedOut)?
                    .map_err(|_| ToolErrorKind::Network)?
            }
        };
        validate_headers(response.headers())?;
        if !response.status().is_success() {
            return Err(classify_status(response.status()));
        }
        validate_content_type(response.headers())?;
        if let Some(value) = response.headers().get(CONTENT_LENGTH) {
            let length = value
                .to_str()
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or(ToolErrorKind::InvalidResponse)?;
            if length > MAX_WEB_SEARCH_BODY_BYTES as u64 {
                return Err(ToolErrorKind::OutputLimit);
            }
        }
        let body = read_body(response.into_body(), deadline, &mut cancellation).await?;
        decode_response(&body)
    }
}

fn search_uri(endpoint: &str, query: &str) -> Result<Uri, ToolErrorKind> {
    let mut value = String::with_capacity(endpoint.len() + query.len() * 3 + 64);
    value.push_str(endpoint);
    value.push_str("?q=");
    percent_encode(query, &mut value);
    value.push_str("&count=10&safesearch=moderate&spellcheck=1");
    value.parse().map_err(|_| ToolErrorKind::InvalidResponse)
}

fn percent_encode(input: &str, output: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            output.push(char::from(byte));
        } else {
            output.push('%');
            output.push(char::from(HEX[usize::from(byte >> 4)]));
            output.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
}

fn validate_headers(headers: &http::HeaderMap) -> Result<(), ToolErrorKind> {
    if headers.len() > MAX_RESPONSE_HEADERS {
        return Err(ToolErrorKind::InvalidResponse);
    }
    let total = headers.iter().try_fold(0_usize, |total, (name, value)| {
        total
            .checked_add(name.as_str().len())?
            .checked_add(value.as_bytes().len())
    });
    if total.is_none_or(|bytes| bytes > MAX_RESPONSE_HEADER_BYTES) {
        return Err(ToolErrorKind::InvalidResponse);
    }
    Ok(())
}

fn validate_content_type(headers: &http::HeaderMap) -> Result<(), ToolErrorKind> {
    let media_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim);
    if !matches!(
        media_type,
        Some("application/json" | "application/x-javascript")
    ) {
        return Err(ToolErrorKind::InvalidResponse);
    }
    Ok(())
}

const fn classify_status(status: StatusCode) -> ToolErrorKind {
    match status.as_u16() {
        408 | 429 | 500..=599 => ToolErrorKind::Network,
        _ => ToolErrorKind::InvalidResponse,
    }
}

async fn read_body(
    mut body: Incoming,
    deadline: Instant,
    cancellation: &mut ProviderCancellation,
) -> Result<Vec<u8>, ToolErrorKind> {
    let mut output = BytesMut::new();
    loop {
        if Instant::now() >= deadline {
            return Err(ToolErrorKind::TimedOut);
        }
        let frame_deadline = deadline.min(Instant::now() + INACTIVITY_TIMEOUT);
        let frame = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(ToolErrorKind::Cancelled),
            frame = time::timeout_at(frame_deadline, body.frame()) => {
                frame.map_err(|_| {
                    if Instant::now() >= deadline {
                        ToolErrorKind::TimedOut
                    } else {
                        ToolErrorKind::InactivityTimeout
                    }
                })?
            }
        };
        let Some(frame) = frame else { break };
        let data = frame
            .map_err(|_| ToolErrorKind::Network)?
            .into_data()
            .map_err(|_| ToolErrorKind::InvalidResponse)?;
        if output
            .len()
            .checked_add(data.len())
            .is_none_or(|length| length > MAX_WEB_SEARCH_BODY_BYTES)
        {
            return Err(ToolErrorKind::OutputLimit);
        }
        output.extend_from_slice(&data);
    }
    Ok(output.to_vec())
}

#[derive(Deserialize)]
struct SearchResponse {
    web: Option<SearchWeb>,
}

#[derive(Deserialize)]
struct SearchWeb {
    results: Vec<SearchResult>,
}

#[derive(Deserialize)]
struct SearchResult {
    title: String,
    url: String,
    description: String,
}

fn decode_response(body: &[u8]) -> Result<Vec<WebSearchResult>, ToolErrorKind> {
    let value = parse_strict_value(body).map_err(|_| ToolErrorKind::InvalidResponse)?;
    let response: SearchResponse =
        serde_json::from_value(value).map_err(|_| ToolErrorKind::InvalidResponse)?;
    let source = response.web.map_or_else(Vec::new, |web| web.results);
    if source.len() > MAX_WEB_SEARCH_RESULTS * 4 {
        return Err(ToolErrorKind::InvalidResponse);
    }
    let mut results = Vec::with_capacity(source.len().min(MAX_WEB_SEARCH_RESULTS));
    let mut urls = BTreeSet::new();
    for result in source {
        push_result(
            &mut results,
            &mut urls,
            &result.title,
            &result.url,
            &result.description,
        );
        if results.len() == MAX_WEB_SEARCH_RESULTS {
            break;
        }
    }
    Ok(results)
}

fn push_result(
    results: &mut Vec<WebSearchResult>,
    urls: &mut BTreeSet<String>,
    title: &str,
    url: &str,
    snippet: &str,
) {
    if results.len() == MAX_WEB_SEARCH_RESULTS || !valid_url(url) || !urls.insert(url.to_owned()) {
        return;
    }
    let title = normalize_text(title, MAX_WEB_SEARCH_TITLE_BYTES);
    let snippet = normalize_text(snippet, MAX_WEB_SEARCH_SNIPPET_BYTES);
    if title.is_empty() && snippet.is_empty() {
        return;
    }
    results.push(WebSearchResult {
        title,
        url: url.to_owned(),
        snippet,
    });
}

fn normalize_text(input: &str, maximum: usize) -> String {
    let mut output = String::with_capacity(input.len().min(maximum));
    for part in input.split_whitespace() {
        let needed = part.len() + usize::from(!output.is_empty());
        if output.len().saturating_add(needed) > maximum {
            break;
        }
        if !output.is_empty() {
            output.push(' ');
        }
        output.push_str(part);
    }
    output
}

fn valid_url(value: &str) -> bool {
    if value.is_empty()
        || value.len() > MAX_WEB_SEARCH_URL_BYTES
        || !value.is_ascii()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return false;
    }
    value.parse::<Uri>().is_ok_and(|uri| {
        matches!(uri.scheme_str(), Some("http" | "https")) && uri.authority().is_some()
    })
}

#[cfg(test)]
mod tests {
    use tokio::{
        io::{AsyncReadExt as _, AsyncWriteExt as _},
        net::TcpListener,
        sync::oneshot,
    };

    use super::*;
    use crate::provider::provider_cancellation;

    #[test]
    fn query_encoding_and_response_decoding_are_bounded() {
        let uri = search_uri(
            "https://api.search.brave.com/res/v1/web/search",
            "rust & tokio",
        )
        .expect("query should encode");
        assert!(uri.to_string().contains("q=rust%20%26%20tokio"));
        assert!(
            uri.to_string()
                .ends_with("count=10&safesearch=moderate&spellcheck=1")
        );
        let body = br#"{
            "web":{"results":[
                {"title":"Rust","url":"https://www.rust-lang.org/","description":"A systems language."},
                {"title":"Alpha","url":"https://example.com/a","description":"First result"},
                {"title":"Beta","url":"https://example.com/b","description":"Second result"}
            ]}
        }"#;
        let results = decode_response(body).expect("response should decode");
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].url, "https://www.rust-lang.org/");
        assert_eq!(results[2].title, "Beta");
        assert!(decode_response(br#"{"web":null,"web":null}"#).is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn adapter_uses_fixed_get_shape_and_honors_cancellation() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture should bind");
        let address = listener.local_addr().expect("fixture address should load");
        let (captured_sender, captured_receiver) = oneshot::channel();
        let fixture = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("request should connect");
            let mut request = vec![0_u8; 4096];
            let bytes = socket
                .read(&mut request)
                .await
                .expect("request should read");
            let _ = captured_sender.send(String::from_utf8_lossy(&request[..bytes]).into_owned());
            let body = r#"{"web":{"results":[{"title":"Source","url":"https://example.com/","description":"Result text"}]}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("response should write");
        });
        let executor = WebSearchToolExecutor::for_test(format!("http://{address}/search"));
        let (_, cancellation) = provider_cancellation();
        let result = executor
            .execute(
                &ToolInput::WebSearch {
                    query: "rust search".to_owned(),
                },
                &cancellation,
            )
            .await;
        assert!(matches!(
            result,
            ToolResult::Ok {
                output: ToolOutput::WebSearch { ref results, .. }
            } if results.len() == 1
        ));
        let request = captured_receiver
            .await
            .expect("request capture should complete");
        assert!(request.starts_with(
            "GET /search?q=rust%20search&count=10&safesearch=moderate&spellcheck=1 HTTP/1.1"
        ));
        assert!(request.contains("x-subscription-token: not-a-real-search-key"));
        fixture.await.expect("fixture should finish");

        let (handle, cancellation) = provider_cancellation();
        handle.cancel();
        assert_eq!(
            executor
                .execute(
                    &ToolInput::WebSearch {
                        query: "cancelled".to_owned(),
                    },
                    &cancellation,
                )
                .await,
            ToolResult::error(ToolErrorKind::Cancelled)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_closes_an_in_flight_search_request() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture should bind");
        let address = listener.local_addr().expect("fixture address should load");
        let (dispatched_sender, dispatched_receiver) = oneshot::channel();
        let fixture = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("request should connect");
            let mut request = vec![0_u8; 4096];
            let bytes = socket
                .read(&mut request)
                .await
                .expect("request should read");
            assert!(bytes > 0);
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n",
                )
                .await
                .expect("response headers should write");
            dispatched_sender
                .send(())
                .unwrap_or_else(|_| panic!("dispatch should be observed"));
            let mut byte = [0_u8; 1];
            match time::timeout(Duration::from_secs(5), socket.read(&mut byte)).await {
                Ok(Ok(0)) => {}
                Ok(Err(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::BrokenPipe
                    ) => {}
                Ok(Ok(_)) => panic!("cancelled request should not send more bytes"),
                Ok(Err(error)) => panic!("cancelled request closed unexpectedly: {error}"),
                Err(_) => panic!("cancelled request should close promptly"),
            }
        });
        let executor = WebSearchToolExecutor::for_test(format!("http://{address}/search"));
        let (handle, cancellation) = provider_cancellation();
        let execution = tokio::spawn(async move {
            executor
                .execute(
                    &ToolInput::WebSearch {
                        query: "cancel in flight".to_owned(),
                    },
                    &cancellation,
                )
                .await
        });
        dispatched_receiver
            .await
            .expect("request should reach response body");
        handle.cancel();
        assert_eq!(
            execution.await.expect("execution should join"),
            ToolResult::error(ToolErrorKind::Cancelled)
        );
        fixture.await.expect("fixture should finish");
    }
}
