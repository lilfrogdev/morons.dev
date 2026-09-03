use std::{collections::BTreeMap, io::Read as _, sync::Arc, time::Duration};

use morons_protocol::{MAX_OPENCODE_API_KEY_BYTES, OpenCodeApiKey};
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::{Mutex, oneshot},
    task::JoinHandle,
    time,
};
use zeroize::Zeroizing;

use super::{
    EndpointSet, GO_CATALOG_URI, GO_INFERENCE_URI, OpenCodeClient, ZEN_CATALOG_URI,
    ZEN_INFERENCE_URI, authorization_header,
};
use crate::provider::{
    OpenCodeResponseRequest, OpenCodeService, ProviderError, ProviderInputItem,
    ProviderMessageRole, ProviderOutputItem, ProviderStreamEvent, provider_cancellation,
};

const TEST_KEY: &str = "not-a-real-provider-key";

struct CapturedRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn request() -> OpenCodeResponseRequest {
    request_for(OpenCodeService::Zen)
}

fn request_for(service: OpenCodeService) -> OpenCodeResponseRequest {
    OpenCodeResponseRequest::new(
        [0x41; 16],
        service,
        "gpt-5.6-luna",
        32,
        128,
        vec![ProviderInputItem::Message {
            role: ProviderMessageRole::User,
            text: "hello".to_owned(),
            phase: None,
        }],
        Vec::new(),
    )
    .expect("test request should be valid")
}

fn endpoints(base: &str) -> EndpointSet {
    EndpointSet {
        zen_inference: format!("{base}/zen/v1/responses"),
        zen_catalog: format!("{base}/zen/v1/models"),
        go_inference: format!("{base}/zen/go/v1/responses"),
        go_catalog: format!("{base}/zen/go/v1/models"),
    }
}

async fn spawn_single_response(
    response: Vec<u8>,
) -> (String, oneshot::Receiver<CapturedRequest>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind");
    let address = listener
        .local_addr()
        .expect("test listener should have an address");
    let (request_sender, request_receiver) = oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("test client should connect");
        let request = read_request(&mut stream).await;
        request_sender
            .send(request)
            .unwrap_or_else(|_| panic!("test should receive request"));
        stream
            .write_all(&response)
            .await
            .expect("test response should be written");
        stream.shutdown().await.expect("test response should close");
    });
    (format!("http://{address}"), request_receiver, task)
}

async fn read_request(stream: &mut TcpStream) -> CapturedRequest {
    let mut received = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 4096];
        let bytes = stream
            .read(&mut chunk)
            .await
            .expect("test request should be readable");
        assert_ne!(bytes, 0, "test request ended before its headers");
        received.extend_from_slice(&chunk[..bytes]);
        assert!(received.len() <= 5 * 1024 * 1024);
        if let Some(position) = received.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let headers =
        std::str::from_utf8(&received[..header_end]).expect("test request headers should be UTF-8");
    let mut lines = headers.split("\r\n");
    let mut request_line = lines
        .next()
        .expect("test request should have a request line")
        .split_whitespace();
    let method = request_line
        .next()
        .expect("test request should have a method")
        .to_owned();
    let path = request_line
        .next()
        .expect("test request should have a path")
        .to_owned();
    let mut parsed_headers = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .expect("test request header should have a separator");
        parsed_headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
    }
    let content_length = parsed_headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    while received.len() - header_end < content_length {
        let mut chunk = [0_u8; 4096];
        let bytes = stream
            .read(&mut chunk)
            .await
            .expect("test request body should be readable");
        assert_ne!(bytes, 0, "test request ended before its body");
        received.extend_from_slice(&chunk[..bytes]);
    }
    CapturedRequest {
        method,
        path,
        headers: parsed_headers,
        body: received[header_end..header_end + content_length].to_vec(),
    }
}

fn response(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    response
}

fn successful_stream() -> Vec<u8> {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"sequence_number\":0,\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"gpt-5.6-luna\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"model\":\"gpt-5.6-luna\",\"status\":\"completed\",\"output\":[{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"hello\",\"annotations\":[]}]}],\"usage\":{\"input_tokens\":3,\"input_tokens_details\":{\"cached_tokens\":0},\"output_tokens\":1,\"output_tokens_details\":{\"reasoning_tokens\":0},\"total_tokens\":4}}}\n\n",
        "data: [DONE]\n\n"
    );
    response("200 OK", "text/event-stream", body.as_bytes())
}

#[tokio::test(flavor = "current_thread")]
async fn inference_uses_the_fixed_service_path_and_scopes_authorization() {
    let (base, captured, server) = spawn_single_response(successful_stream()).await;
    let client = OpenCodeClient::for_test(endpoints(&base));
    let (_handle, mut cancellation) = provider_cancellation();
    let outcome = client
        .execute_for_test(TEST_KEY.as_bytes(), &request(), &mut cancellation, |_| {})
        .await
        .expect("test inference should complete");
    assert_eq!(outcome.provider_response_id, "resp_1");

    let captured = captured.await.expect("request should be captured");
    assert_eq!(captured.method, "POST");
    assert_eq!(captured.path, "/zen/v1/responses");
    assert_eq!(
        captured.headers.get("authorization").map(String::as_str),
        Some("Bearer not-a-real-provider-key")
    );
    assert_eq!(
        captured.headers.get("accept").map(String::as_str),
        Some("text/event-stream")
    );
    assert_eq!(
        captured.headers.get("user-agent").map(String::as_str),
        Some(concat!("morons-server/", env!("CARGO_PKG_VERSION")))
    );
    assert_eq!(
        captured
            .headers
            .get("x-opencode-session")
            .map(String::as_str),
        Some("ses_a226742b98d72df1c38a2e7016096028")
    );
    let body: Value = serde_json::from_slice(&captured.body).expect("request body should be JSON");
    assert_eq!(body["model"], "gpt-5.6-luna");
    server.await.expect("test server should finish");

    let (base, captured, server) = spawn_single_response(successful_stream()).await;
    let client = OpenCodeClient::for_test(endpoints(&base));
    let (_handle, mut cancellation) = provider_cancellation();
    client
        .execute_for_test(
            TEST_KEY.as_bytes(),
            &request_for(OpenCodeService::Go),
            &mut cancellation,
            |_| {},
        )
        .await
        .expect("Go inference should complete");
    let captured = captured.await.expect("Go request should be captured");
    assert_eq!(captured.path, "/zen/go/v1/responses");
    assert_eq!(
        captured
            .headers
            .get("x-opencode-session")
            .map(String::as_str),
        Some("ses_a226742b98d72df1c38a2e7016096028")
    );
    server.await.expect("Go test server should finish");
}

#[tokio::test(flavor = "current_thread")]
async fn public_catalog_requests_never_receive_authorization() {
    let catalog = br#"{"object":"list","data":[{"id":"grok-4.6","object":"model","created":1,"owned_by":"opencode"}]}"#;
    let (base, captured, server) =
        spawn_single_response(response("200 OK", "application/json", catalog)).await;
    let client = OpenCodeClient::for_test(endpoints(&base));
    let models = client
        .fetch_catalog(OpenCodeService::Zen)
        .await
        .expect("catalog should decode");
    assert!(
        models
            .iter()
            .any(|entry| entry.model.id == "grok-4.6" && entry.available)
    );

    let captured = captured.await.expect("request should be captured");
    assert_eq!(captured.method, "GET");
    assert_eq!(captured.path, "/zen/v1/models");
    assert!(!captured.headers.contains_key("authorization"));
    assert!(!captured.headers.contains_key("x-opencode-session"));
    assert!(captured.body.is_empty());
    server.await.expect("test server should finish");
}

#[tokio::test(flavor = "current_thread")]
async fn redirects_and_unexpected_content_types_fail_closed() {
    let redirect = b"HTTP/1.1 302 Found\r\nLocation: http://example.invalid/steal\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
    let (base, _captured, server) = spawn_single_response(redirect).await;
    let client = OpenCodeClient::for_test(endpoints(&base));
    let (_handle, mut cancellation) = provider_cancellation();
    assert_eq!(
        client
            .execute_for_test(TEST_KEY.as_bytes(), &request(), &mut cancellation, |_| {})
            .await,
        Err(ProviderError::RedirectDenied)
    );
    server.await.expect("redirect server should finish");

    let (base, _captured, server) =
        spawn_single_response(response("200 OK", "application/json", b"{}")).await;
    let client = OpenCodeClient::for_test(endpoints(&base));
    let (_handle, mut cancellation) = provider_cancellation();
    assert_eq!(
        client
            .execute_for_test(TEST_KEY.as_bytes(), &request(), &mut cancellation, |_| {})
            .await,
        Err(ProviderError::UnexpectedContentType)
    );
    server.await.expect("content-type server should finish");
}

#[tokio::test(flavor = "current_thread")]
async fn inference_is_not_retried_after_a_provider_failure() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind");
    let address = listener.local_addr().expect("listener should have address");
    let connections = Arc::new(Mutex::new(0_usize));
    let observed_connections = Arc::clone(&connections);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("client should connect");
        *observed_connections.lock().await += 1;
        let _ = read_request(&mut stream).await;
        stream
            .write_all(&response(
                "503 Service Unavailable",
                "application/json",
                b"{}",
            ))
            .await
            .expect("failure response should write");
        stream
            .shutdown()
            .await
            .expect("failure response should close");
        if let Ok(Ok((_stream, _))) =
            time::timeout(Duration::from_millis(200), listener.accept()).await
        {
            *observed_connections.lock().await += 1;
        }
    });
    let base = format!("http://{address}");
    let client = OpenCodeClient::for_test(endpoints(&base));
    let (_handle, mut cancellation) = provider_cancellation();
    assert_eq!(
        client
            .execute_for_test(TEST_KEY.as_bytes(), &request(), &mut cancellation, |_| {})
            .await,
        Err(ProviderError::Unavailable)
    );
    server.await.expect("retry observer should finish");
    assert_eq!(*connections.lock().await, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_aborts_an_active_response_stream() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind");
    let address = listener.local_addr().expect("listener should have address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("client should connect");
        let _ = read_request(&mut stream).await;
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .await
            .expect("stream headers should write");
        time::sleep(Duration::from_secs(2)).await;
    });
    let base = format!("http://{address}");
    let client = OpenCodeClient::for_test(endpoints(&base));
    let (handle, mut cancellation) = provider_cancellation();
    let cancellation_task = tokio::spawn(async move {
        time::sleep(Duration::from_millis(25)).await;
        handle.cancel();
    });
    assert_eq!(
        client
            .execute_for_test(TEST_KEY.as_bytes(), &request(), &mut cancellation, |_| {})
            .await,
        Err(ProviderError::Cancelled)
    );
    cancellation_task
        .await
        .expect("cancellation task should finish");
    server.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn response_headers_error_bodies_and_tls_failures_are_bounded() {
    let mut too_many_headers =
        String::from("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n");
    for index in 0..=super::MAX_RESPONSE_HEADERS {
        too_many_headers.push_str(&format!("X-Test-{index}: value\r\n"));
    }
    too_many_headers.push_str("Content-Length: 0\r\nConnection: close\r\n\r\n");
    let (base, _captured, server) = spawn_single_response(too_many_headers.into_bytes()).await;
    let client = OpenCodeClient::for_test(endpoints(&base));
    let (_handle, mut cancellation) = provider_cancellation();
    assert_eq!(
        client
            .execute_for_test(TEST_KEY.as_bytes(), &request(), &mut cancellation, |_| {})
            .await,
        Err(ProviderError::ResponseLimitExceeded)
    );
    server.await.expect("header-limit server should finish");

    let oversized_error = vec![b'x'; super::MAX_ERROR_BODY_BYTES + 1];
    let (base, _captured, server) = spawn_single_response(response(
        "401 Unauthorized",
        "application/json",
        &oversized_error,
    ))
    .await;
    let client = OpenCodeClient::for_test(endpoints(&base));
    let (_handle, mut cancellation) = provider_cancellation();
    assert_eq!(
        client
            .execute_for_test(TEST_KEY.as_bytes(), &request(), &mut cancellation, |_| {})
            .await,
        Err(ProviderError::ResponseLimitExceeded)
    );
    server.await.expect("error-limit server should finish");

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("TLS failure listener should bind");
    let address = listener.local_addr().expect("listener should have address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("TLS client should connect");
        let mut data = [0_u8; 1024];
        let _ = stream.read(&mut data).await;
        stream
            .shutdown()
            .await
            .expect("TLS test stream should close");
    });
    let https_base = format!("https://{address}");
    let client = OpenCodeClient::for_test(endpoints(&https_base));
    let (_handle, mut cancellation) = provider_cancellation();
    assert_eq!(
        client
            .execute_for_test(TEST_KEY.as_bytes(), &request(), &mut cancellation, |_| {})
            .await,
        Err(ProviderError::Transport)
    );
    server.await.expect("TLS failure server should finish");
}

#[test]
fn production_endpoints_are_exact_and_non_configurable() {
    let authorization = authorization_header(TEST_KEY.as_bytes())
        .expect("test authorization header should be valid");
    assert!(authorization.is_sensitive());
    assert!(!format!("{authorization:?}").contains(TEST_KEY));

    let endpoints = EndpointSet::production();
    assert_eq!(endpoints.zen_inference, ZEN_INFERENCE_URI);
    assert_eq!(endpoints.zen_catalog, ZEN_CATALOG_URI);
    assert_eq!(endpoints.go_inference, GO_INFERENCE_URI);
    assert_eq!(endpoints.go_catalog, GO_CATALOG_URI);
}

fn read_live_api_key() -> Zeroizing<Vec<u8>> {
    let mut input = Zeroizing::new(Vec::new());
    std::io::stdin()
        .lock()
        .take((MAX_OPENCODE_API_KEY_BYTES + 3) as u64)
        .read_to_end(&mut input)
        .expect("live API key should be readable from stdin");
    if input.last() == Some(&b'\n') {
        input.pop();
        if input.last() == Some(&b'\r') {
            input.pop();
        }
    }
    let api_key = OpenCodeApiKey::new(std::mem::take(&mut *input))
        .expect("stdin must contain one bounded OpenCode API key");
    Zeroizing::new(api_key.into_bytes())
}

fn live_request(service: OpenCodeService, model: &str) -> OpenCodeResponseRequest {
    OpenCodeResponseRequest::new(
        [0x42; 16],
        service,
        model,
        1_024,
        512,
        vec![ProviderInputItem::Message {
            role: ProviderMessageRole::User,
            text: "Reply with the single word OK.".to_owned(),
            phase: None,
        }],
        Vec::new(),
    )
    .expect("live request should use a reviewed model")
}

async fn run_live_contract_case(
    client: &OpenCodeClient,
    api_key: &[u8],
    service: OpenCodeService,
    model: &str,
) {
    let catalog = client
        .fetch_catalog(service)
        .await
        .unwrap_or_else(|error| panic!("live {} catalog failed: {error}", service.as_str()));
    assert!(
        catalog
            .iter()
            .any(|entry| entry.model.id == model && entry.available),
        "reviewed live model is unavailable for {}",
        service.as_str()
    );

    let request = live_request(service, model);
    let (_handle, mut cancellation) = provider_cancellation();
    let mut events = Vec::new();
    let outcome = client
        .execute_for_test(api_key, &request, &mut cancellation, |event| {
            events.push(event);
        })
        .await
        .unwrap_or_else(|error| panic!("live {} inference failed: {error}", service.as_str()));

    assert!(
        events
            .iter()
            .any(|event| matches!(event, ProviderStreamEvent::TextDelta { .. })),
        "live {} response did not stream a text delta",
        service.as_str()
    );
    assert!(
        outcome.output.iter().any(|item| matches!(
            item,
            ProviderOutputItem::AssistantMessage(message)
                if !message.refusal && !message.text.trim().is_empty()
        )),
        "live {} response did not contain assistant text",
        service.as_str()
    );
    assert!(
        !outcome
            .output
            .iter()
            .any(|item| matches!(item, ProviderOutputItem::ToolCall(_))),
        "live {} response returned an undeclared tool call",
        service.as_str()
    );
    assert!(outcome.usage.input_tokens > 0);
    assert!(outcome.usage.output_tokens > 0);
    assert_eq!(
        outcome.usage.total_tokens,
        outcome.usage.input_tokens + outcome.usage.output_tokens
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a real OpenCode Zen key on stdin and makes one billable request"]
async fn live_opencode_zen_contract() {
    let api_key = read_live_api_key();
    let client = OpenCodeClient::for_live_test();
    run_live_contract_case(&client, &api_key, OpenCodeService::Zen, "muse-spark-1.2").await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a real OpenCode Go key on stdin and makes one billable request"]
async fn live_opencode_go_contract() {
    let api_key = read_live_api_key();
    let client = OpenCodeClient::for_live_test();
    run_live_contract_case(&client, &api_key, OpenCodeService::Go, "grok-4.6").await;
}
