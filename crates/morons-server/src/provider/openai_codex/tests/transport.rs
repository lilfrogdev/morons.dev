use super::*;
mod boundaries;
mod diagnostics;
mod media_type;
mod models;
use crate::{
    persistence::{MutationRequestId, SessionStore, credential_tests::TestRoot},
    provider::{
        ProviderOutputItem,
        openai_auth::{OAuthTokens, OpenAiCredentialProvider, tests::mock_request},
        provider_cancellation,
    },
};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    time,
};

fn tokens() -> OAuthTokens {
    OAuthTokens::fixture(
        "codex-account-fixture",
        "codex-refresh-fixture",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600,
    )
}
async fn setup() -> (
    TestRoot,
    Arc<SessionStore>,
    Arc<OpenAiCodexProvider>,
    TcpListener,
) {
    let root = TestRoot::new("codex-adapter");
    let store = Arc::new(SessionStore::open_for_test(root.path()).unwrap());
    store
        .set_openai_credential(MutationRequestId::from_bytes([1; 16]), 0, tokens())
        .await
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = Arc::new(OpenAiCodexProvider::for_test(
        Arc::new(OpenAiCredentialProvider::new(store.clone())),
        format!(
            "http://{}/backend-api/codex/responses",
            listener.local_addr().unwrap()
        )
        .parse()
        .unwrap(),
    ));
    (root, store, provider, listener)
}
fn sse(output_tokens: u32) -> Vec<u8> {
    let created = json!({"type":"response.created","sequence_number":0,"response":{"id":"resp_fixture","object":"response","status":"in_progress","model":"gpt-5.5"}});
    let completed = json!({"type":"response.completed","sequence_number":1,"response":{"id":"resp_fixture","object":"response","status":"completed","model":"gpt-5.5","output":[{"id":"msg_fixture","type":"message","role":"assistant","status":"completed","phase":"final_answer","content":[{"type":"output_text","text":"fixture answer","annotations":[]}]},{"id":"rs_fixture","type":"reasoning","summary":[],"encrypted_content":"opaque-response-fixture"},{"id":"fc_fixture","type":"function_call","status":"completed","call_id":"call_fixture","name":"read","arguments":"{}"}],"usage":{"input_tokens":10,"input_tokens_details":{"cached_tokens":2},"output_tokens":output_tokens,"output_tokens_details":{"reasoning_tokens":1},"total_tokens":10+output_tokens}}});
    format!("data: {created}\n\ndata: {completed}\n\ndata: [DONE]\n\n").into_bytes()
}
fn sse_with_item_events(output_tokens: u32) -> Vec<u8> {
    crate::provider::completed_item_stream_fixture(&String::from_utf8(sse(output_tokens)).unwrap())
        .into_bytes()
}
async fn respond(socket: &mut TcpStream, status: &str, headers: &str, body: &[u8]) {
    socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",body.len()).as_bytes()).await.unwrap();
    socket.write_all(body).await.unwrap();
}
#[tokio::test(flavor = "current_thread")]
async fn native_dispatch_uses_scoped_headers_and_never_replays_prepared_bytes() {
    let (_root, _store, provider, listener) = setup().await;
    let peer = tokio::spawn(async move {
        let mut ids = Vec::new();
        let mut sessions = Vec::new();
        for pass in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let message = mock_request(&mut socket).await;
            let (headers, body) = message.split_once("\r\n\r\n").unwrap();
            let headers = headers.to_lowercase();
            assert!(headers.starts_with("post /backend-api/codex/responses http/1.1"));
            assert!(headers.contains("originator: morons"));
            assert!(headers.contains("chatgpt-account-id: codex-account-fixture"));
            assert!(headers.contains("authorization: bearer "));
            assert!(!headers.contains("x-opencode"));
            assert!(!headers.contains("refresh-fixture"));
            assert_eq!(
                headers.contains("x-codex-turn-state: sticky-fixture"),
                pass == 1
            );
            ids.push(
                headers
                    .lines()
                    .find(|s| s.starts_with("x-client-request-id:"))
                    .unwrap()
                    .to_owned(),
            );
            sessions.push(
                headers
                    .lines()
                    .find(|s| s.starts_with("session-id:"))
                    .unwrap()
                    .to_owned(),
            );
            let body: Value = serde_json::from_str(body).unwrap();
            assert_eq!(body["model"], "gpt-5.5");
            assert!(body.get("max_output_tokens").is_none());
            respond(
                &mut socket,
                "200 OK",
                "x-codex-turn-state: sticky-fixture\r\n",
                &if pass == 0 {
                    sse_with_item_events(5)
                } else {
                    sse(5)
                },
            )
            .await;
        }
        assert_ne!(ids[0], ids[1]);
        assert_eq!(sessions[0], sessions[1]);
        listener
    });
    let mut turn = provider.new_turn([1; 16], [2; 16], 1, "gpt-5.5").unwrap();
    let first = request(&turn);
    let (_, mut cancel) = provider_cancellation();
    for pass in 0..2 {
        let request = if pass == 0 { &first } else { &request(&turn) };
        let outcome = provider
            .prepare_dispatch(
                &mut turn,
                request,
                DataUseRestrictions::default(),
                &mut cancel,
            )
            .await
            .unwrap()
            .execute(DataUseRestrictions::default(), &mut cancel, |_| {})
            .await
            .unwrap();
        assert_eq!(outcome.usage.total_tokens, 15);
        assert!(
            outcome
                .output
                .iter()
                .any(|o| matches!(o, ProviderOutputItem::Reasoning(_)))
        );
        assert!(
            outcome
                .output
                .iter()
                .any(|o| matches!(o, ProviderOutputItem::ToolCall(_)))
        );
        assert!(!format!("{outcome:?} {turn:?}").contains("opaque-response-fixture"));
        assert!(!format!("{turn:?}").contains("sticky-fixture"));
    }
    assert!(
        provider
            .prepare_dispatch(
                &mut turn,
                &first,
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
async fn failed_cancelled_and_dropped_dispatches_poison_the_turn_without_retry() {
    for failure in [
        "redirect",
        "entitlement",
        "truncated",
        "usage",
        "routing",
        "cancel",
        "drop",
    ] {
        let (_root, _store, provider, listener) = setup().await;
        let mut turn = provider.new_turn([1; 16], [2; 16], 1, "gpt-5.5").unwrap();
        let request = request(&turn);
        let (handle, mut cancel) = provider_cancellation();
        let (sent, received) = oneshot::channel();
        let peer = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _ = mock_request(&mut socket).await;
            sent.send(()).unwrap();
            match failure {
                "redirect" => {
                    respond(
                        &mut socket,
                        "302 Found",
                        "Location: http://127.0.0.1:1/other\r\n",
                        b"",
                    )
                    .await
                }
                "entitlement" => {
                    respond(
                        &mut socket,
                        "401 Unauthorized",
                        "",
                        b"opaque error never reflected",
                    )
                    .await
                }
                "truncated" => {
                    respond(
                        &mut socket,
                        "200 OK",
                        "",
                        b"data: {\"type\":\"response.created\"}\n\n",
                    )
                    .await
                }
                "usage" => respond(&mut socket, "200 OK", "", &sse(33)).await,
                "routing" => {
                    respond(
                        &mut socket,
                        "200 OK",
                        "x-codex-turn-state: first\r\nx-codex-turn-state: second\r\n",
                        &sse(5),
                    )
                    .await
                }
                _ => {
                    let mut byte = [0];
                    assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
                }
            }
            listener
        });
        let dispatch = provider
            .prepare_dispatch(
                &mut turn,
                &request,
                DataUseRestrictions::default(),
                &mut cancel,
            )
            .await
            .unwrap();
        let mut execute =
            Box::pin(dispatch.execute(DataUseRestrictions::default(), &mut cancel, |_| {}));
        if failure == "drop" {
            tokio::select! {_=&mut execute=>panic!("blocked response expected"),_=received=>{}};
        } else {
            let trigger = async {
                let _ = received.await;
                if failure == "cancel" {
                    handle.cancel();
                }
            };
            let (result, ()) = tokio::join!(&mut execute, trigger);
            assert!(result.is_err());
        }
        drop(execute);
        assert!(!turn.usable);
        if matches!(failure, "cancel" | "drop" | "entitlement") {
            assert_eq!(turn.response_failure(), None);
        }
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
