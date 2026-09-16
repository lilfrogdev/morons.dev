use super::*;
use crate::provider::provider_cancellation;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

fn payload() -> Value {
    json!({"jsonrpc":"2.0","id":1,"result":{"structuredContent":{"results":[{
        "title":"Example", "url":"https://example.com/page", "text":"Source excerpt"
    }]}}})
}

#[test]
fn decodes_json_and_single_complete_sse_record() {
    let body = payload().to_string();
    let expected = decode("query", body.as_bytes(), false).unwrap();
    for prefix in ["", "event: message\r\n"] {
        let sse = format!("{prefix}data: {body}\r\n\r\n");
        assert_eq!(decode("query", sse.as_bytes(), true).unwrap(), expected);
    }
    assert_eq!(expected.query, "query");
    assert_eq!(expected.results[0].snippet, "Source excerpt");
    assert!(expected.summary().contains("token usage not provided"));
}

#[test]
fn decodes_text_and_json_text_content() {
    for text in [
        "Title: Example\nURL: https://example.com/page\nText: Source excerpt".to_owned(),
        "Title: Example\nURL: https://example.com/page\nHighlights:\nSource excerpt".to_owned(),
        payload()["result"]["structuredContent"].to_string(),
    ] {
        let rpc =
            json!({"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":text}]}});
        let result = decode("query", rpc.to_string().as_bytes(), false).unwrap();
        assert_eq!(result.results.len(), 1);
        assert_eq!(result.results[0].snippet, "Source excerpt");
    }
}

#[test]
fn rejects_invalid_rpc_and_results() {
    let mut cases = Vec::new();
    for (key, value) in [
        ("id", json!(2)),
        ("id", json!("1")),
        ("jsonrpc", json!("1.0")),
        ("error", json!({"code":-1})),
    ] {
        let mut rpc = payload();
        rpc[key] = value;
        cases.push(rpc);
    }
    for value in [json!(true), json!(null), json!("false")] {
        let mut rpc = payload();
        rpc["result"]["isError"] = value;
        cases.push(rpc);
    }
    for value in [
        json!([]),
        json!([{}]),
        json!(vec![
            json!({"title":"x","url":"https://example.com","text":"x"});
            11
        ]),
    ] {
        let mut rpc = payload();
        rpc["result"]["structuredContent"]["results"] = value;
        cases.push(rpc);
    }
    for (key, value) in [
        ("url", "javascript:alert(1)"),
        ("title", " "),
        ("text", " "),
    ] {
        let mut rpc = payload();
        rpc["result"]["structuredContent"]["results"][0][key] = json!(value);
        cases.push(rpc);
    }
    for rpc in cases {
        assert!(
            decode("query", rpc.to_string().as_bytes(), false).is_err(),
            "{rpc}"
        );
    }
    assert!(
        decode(
            "query",
            br#"{"jsonrpc":"2.0","id":1,"id":1,"result":{}}"#,
            false
        )
        .is_err()
    );
    assert!(decode("query", b"", false).is_err());
    assert!(decode("", payload().to_string().as_bytes(), false).is_err());
}

#[test]
fn rejects_incomplete_extra_and_wrong_sse_events() {
    let body = payload().to_string();
    for sse in [
        format!("data: {body}\n"),
        format!("data: {body}\n\ndata: {body}\n\n"),
        format!("event: other\ndata: {body}\n\n"),
    ] {
        assert!(decode("query", sse.as_bytes(), true).is_err());
    }
}

#[test]
fn bounds_response_and_utf8_excerpts_and_privacy() {
    assert_eq!(
        decode("query", &vec![b' '; MAX_RESPONSE_BYTES + 1], false).unwrap_err(),
        ProviderError::ResponseLimitExceeded
    );
    let text = "€".repeat(crate::tools::MAX_WEB_SEARCH_SNIPPET_BYTES);
    let snippet = excerpt(&text);
    assert!(snippet.len() <= crate::tools::MAX_WEB_SEARCH_SNIPPET_BYTES);
    assert!(text.starts_with(&snippet));
    for training in [false, true] {
        for retention in [false, true] {
            assert_eq!(
                permits(DataUseRestrictions {
                    block_training_use: training,
                    require_zero_retention: retention
                }),
                !training && !retention
            );
        }
    }
}

async fn fixture(response: Vec<u8>, stall: bool) -> (ExaProvider, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let expected = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"web_search_exa","arguments":{"query":"query","numResults":5}}}).to_string();
        while !request.ends_with(expected.as_bytes()) {
            let mut buffer = [0; 4096];
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            request.extend_from_slice(&buffer[..count]);
            assert!(request.len() < 8192);
        }
        let request = String::from_utf8(request).unwrap();
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
        socket.write_all(&response).await.unwrap();
        if stall {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        drop(socket);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    });
    let provider = ExaProvider {
        client: bounded_client(
            true,
            Some((MAX_RESPONSE_HEADERS, MAX_RESPONSE_HEADER_BYTES)),
        ),
        endpoint: format!("http://{address}/mcp?tools=web_search_exa")
            .parse()
            .unwrap(),
        timeout: Duration::from_secs(2),
    };
    (provider, server)
}

#[tokio::test]
async fn http_json_and_sse() {
    let body = payload().to_string();
    for (content_type, body) in [
        ("application/json", body.clone()),
        (
            "text/event-stream",
            format!("event: message\ndata: {body}\n\n"),
        ),
    ] {
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (provider, server) = fixture(response.into_bytes(), false).await;
        let (_, mut cancellation) = provider_cancellation();
        assert!(
            provider
                .execute("query", &mut cancellation)
                .await
                .unwrap()
                .is_valid()
        );
        server.await.unwrap();
    }
}

#[tokio::test]
async fn http_failures_never_retry_or_follow_redirects() {
    for (response, error) in [
        ("HTTP/1.1 302 Found\r\nLocation: /other\r\nContent-Length: 0\r\n\r\n".to_owned(), ProviderError::RedirectDenied),
        ("HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n".to_owned(), ProviderError::Unavailable),
        (format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", MAX_RESPONSE_BYTES + 1), ProviderError::ResponseLimitExceeded),
        ("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Type: text/event-stream\r\nContent-Length: 0\r\n\r\n".to_owned(), ProviderError::MalformedResponse),
    ] {
        let (provider, server) = fixture(response.into_bytes(), false).await;
        let (_, mut cancellation) = provider_cancellation();
        assert_eq!(provider.execute("query", &mut cancellation).await.unwrap_err(), ExaFailure::Uncertain(error));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn body_deadline_and_cancellation_are_bounded() {
    for cancel in [false, true] {
        let (mut provider, server) = fixture(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{"
                .to_vec(),
            true,
        )
        .await;
        let (handle, mut cancellation) = provider_cancellation();
        let cancellation_task = if cancel {
            Some(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                handle.cancel();
            }))
        } else {
            provider.timeout = Duration::from_millis(50);
            None
        };
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            provider.execute("query", &mut cancellation),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(
            error,
            ExaFailure::Uncertain(if cancel {
                ProviderError::Cancelled
            } else {
                ProviderError::TotalTimeout
            })
        );
        if let Some(task) = cancellation_task {
            task.await.unwrap();
        }
        server.await.unwrap();
    }
}

#[tokio::test]
async fn header_wait_obeys_total_deadline() {
    let (mut provider, server) = fixture(Vec::new(), true).await;
    provider.timeout = Duration::from_millis(50);
    let (_, mut cancellation) = provider_cancellation();
    assert_eq!(
        provider
            .execute("query", &mut cancellation)
            .await
            .unwrap_err(),
        ExaFailure::Uncertain(ProviderError::TotalTimeout)
    );
    server.await.unwrap();
}

#[tokio::test]
async fn precancelled_request_is_not_dispatched() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = ExaProvider {
        client: bounded_client(true, None),
        endpoint: format!("http://{}/mcp", listener.local_addr().unwrap())
            .parse()
            .unwrap(),
        timeout: Duration::from_secs(1),
    };
    let (handle, mut cancellation) = provider_cancellation();
    handle.cancel();
    assert_eq!(
        provider
            .execute("query", &mut cancellation)
            .await
            .unwrap_err(),
        ExaFailure::BeforeDispatch(ProviderError::Cancelled)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}
