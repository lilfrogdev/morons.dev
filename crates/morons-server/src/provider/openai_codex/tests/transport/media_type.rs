use super::*;
use crate::provider::{
    response_diagnostic::ResponseStage, response_http, sse::MAX_PROVIDER_STREAM_BYTES,
};

async fn check_response(
    headers: &str,
    body: Vec<u8>,
    declared_length: Option<usize>,
    failure: Option<(ProviderError, ResponseStage)>,
) {
    let (_root, _store, provider, listener) = setup().await;
    let headers = headers.to_owned();
    let peer = tokio::spawn(async move {
        let (mut socket, _) = time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let message = mock_request(&mut socket).await;
        let (request_headers, request_body) = message.split_once("\r\n\r\n").unwrap();
        assert!(
            request_headers
                .to_ascii_lowercase()
                .contains("accept: text/event-stream")
        );
        let request_body: Value = serde_json::from_str(request_body).unwrap();
        assert_eq!(request_body["stream"], true);
        assert_eq!(request_body["model"], "gpt-5.5");
        let length = declared_length.unwrap_or(body.len());
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n{headers}\r\n"
        )
        .into_bytes();
        response.extend_from_slice(&body);
        socket.write_all(&response).await.unwrap();
        listener
    });
    let mut turn = provider.new_turn([1; 16], [2; 16], 1, "gpt-5.5").unwrap();
    let request = request(&turn);
    let (_, mut cancel) = provider_cancellation();
    let result = provider
        .prepare_dispatch(
            &mut turn,
            &request,
            DataUseRestrictions::default(),
            &mut cancel,
        )
        .await
        .unwrap()
        .execute(DataUseRestrictions::default(), &mut cancel, |_| {})
        .await;
    if let Some((error, stage)) = failure {
        assert_eq!(result.unwrap_err(), error);
        assert_eq!(turn.response_failure(), Some(stage));
        assert!(!turn.usable);
        assert!(!format!("{turn:?} {error:?} {stage:?}").contains("PRIVATE"));
    } else {
        let outcome =
            result.expect("complete strict native SSE does not need a response media type");
        assert_eq!(outcome.usage.total_tokens, 15);
        assert_eq!(outcome.output.len(), 3);
        assert!(
            outcome
                .output
                .iter()
                .any(|item| matches!(item, ProviderOutputItem::Reasoning(_)))
        );
        assert!(
            outcome
                .output
                .iter()
                .any(|item| matches!(item, ProviderOutputItem::ToolCall(_)))
        );
        assert!(turn.usable);
        assert_eq!(turn.response_failure(), None);
    }
    // Failure poisons the turn; success still cannot replay these prepared bytes.
    assert!(
        provider
            .prepare_dispatch(
                &mut turn,
                &request,
                DataUseRestrictions::default(),
                &mut cancel
            )
            .await
            .is_err()
    );
    let listener = peer.await.unwrap();
    assert!(
        time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn native_missing_media_type_accepts_only_complete_reviewed_sse() {
    for headers in [
        "",
        "Content-Type: text/event-stream\r\n",
        "Content-Type: Text/Event-Stream; charset=utf-8\r\n",
    ] {
        check_response(headers, sse(5), None, None).await;
    }
    // The shared helper used by OpenCode and OAuth has not been weakened.
    for expected in ["text/event-stream", "application/json"] {
        assert_eq!(
            response_http::require_content_type(&http::HeaderMap::new(), expected),
            Err(ProviderError::UnexpectedContentType)
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_media_type_omission_never_admits_conflicting_headers_or_invalid_streams() {
    for value in [
        "",
        "application/json",
        "text/html",
        "application/PRIVATE",
        "text/event-stream, text/event-stream",
    ] {
        check_response(
            &format!("Content-Type: {value}\r\n"),
            sse(5),
            None,
            Some((
                ProviderError::UnexpectedContentType,
                ResponseStage::ContentType,
            )),
        )
        .await;
    }
    check_response(
        "Content-Type: text/event-stream\r\nContent-Type: text/event-stream\r\n",
        sse(5),
        None,
        Some((
            ProviderError::MalformedResponse,
            ResponseStage::HeaderFraming,
        )),
    )
    .await;
    for body in [
        b"<html>PRIVATE</html>\n\n".to_vec(),
        b"{\"error\":\"PRIVATE\"}\n\n".to_vec(),
    ] {
        check_response(
            "",
            body,
            None,
            Some((ProviderError::MalformedResponse, ResponseStage::SseFraming)),
        )
        .await;
    }
    for body in [Vec::new(), b"data: {\"type\":\"PRIVATE\"}".to_vec()] {
        check_response(
            "",
            body,
            None,
            Some((
                ProviderError::IncompleteResponse,
                ResponseStage::Termination,
            )),
        )
        .await;
    }
    let invalid_model = String::from_utf8(sse(5))
        .unwrap()
        .replace("gpt-5.5", "PRIVATE")
        .into_bytes();
    check_response(
        "",
        invalid_model,
        None,
        Some((
            ProviderError::MalformedResponse,
            ResponseStage::ResponseModel,
        )),
    )
    .await;
    check_response(
        "",
        sse(33),
        None,
        Some((ProviderError::MalformedResponse, ResponseStage::Usage)),
    )
    .await;
    check_response(
        "",
        Vec::new(),
        Some(MAX_PROVIDER_STREAM_BYTES + 1),
        Some((
            ProviderError::ResponseLimitExceeded,
            ResponseStage::BodyBounds,
        )),
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_untyped_body_cancellation_and_deadline_poison_without_replay() {
    for cancel_body in [false, true] {
        let (_root, _store, mut provider, listener) = setup().await;
        Arc::get_mut(&mut provider).unwrap().set_timeouts_for_test(
            Duration::from_secs(1),
            if cancel_body {
                Duration::from_secs(3)
            } else {
                Duration::from_millis(200)
            },
        );
        let (sent, received) = oneshot::channel();
        let peer = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _ = mock_request(&mut socket).await;
            // Cache complete items but withhold terminal identity/usage. No
            // outcome or tool call can escape on cancellation/timeout.
            let complete = String::from_utf8(sse_with_item_events(5)).unwrap();
            let partial = complete
                .trim_end()
                .rsplit_once("\n\n")
                .unwrap()
                .0
                .to_owned()
                + "\n\n";
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        partial.len() + 100
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            socket.write_all(partial.as_bytes()).await.unwrap();
            sent.send(()).unwrap();
            let mut byte = [0];
            assert_eq!(
                time::timeout(Duration::from_secs(5), socket.read(&mut byte))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
            listener
        });
        let mut turn = provider.new_turn([1; 16], [2; 16], 1, "gpt-5.5").unwrap();
        let request = request(&turn);
        let (handle, mut cancel) = provider_cancellation();
        let dispatch = provider
            .prepare_dispatch(
                &mut turn,
                &request,
                DataUseRestrictions::default(),
                &mut cancel,
            )
            .await
            .unwrap();
        let trigger = async {
            received.await.unwrap();
            if cancel_body {
                time::sleep(Duration::from_millis(30)).await;
                handle.cancel();
            }
        };
        let (result, ()) = tokio::join!(
            dispatch.execute(DataUseRestrictions::default(), &mut cancel, |_| {}),
            trigger
        );
        assert_eq!(
            result.unwrap_err(),
            if cancel_body {
                ProviderError::Cancelled
            } else {
                ProviderError::TotalTimeout
            }
        );
        assert!(!turn.usable);
        assert_eq!(turn.response_failure(), None);
        assert!(
            provider
                .prepare_dispatch(
                    &mut turn,
                    &request,
                    DataUseRestrictions::default(),
                    &mut cancel
                )
                .await
                .is_err()
        );
        let listener = peer.await.unwrap();
        assert!(
            time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }
}
