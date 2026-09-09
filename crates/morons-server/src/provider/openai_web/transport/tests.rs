use super::*;
use crate::{
    persistence::{MutationRequestId, SessionStore, credential_tests::TestRoot},
    provider::{
        openai_auth::{OAuthTokens, tests::mock_request},
        provider_cancellation,
    },
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::{io::AsyncWriteExt as _, net::TcpListener, sync::oneshot};

async fn setup() -> (TestRoot, Arc<SessionStore>, SearchProvider, TcpListener) {
    let root = TestRoot::new("web-transport");
    let store = Arc::new(SessionStore::open_for_test(root.path()).unwrap());
    let expires = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    store
        .set_openai_credential(
            MutationRequestId::from_bytes([1; 16]),
            0,
            OAuthTokens::fixture("web-account-fixture", "web-refresh-fixture", expires),
        )
        .await
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = SearchProvider::for_test(
        Arc::new(OpenAiCredentialProvider::new(store.clone())),
        format!(
            "http://{}/backend-api/codex/responses",
            listener.local_addr().unwrap()
        )
        .parse()
        .unwrap(),
    );
    (root, store, provider, listener)
}
#[tokio::test(flavor = "current_thread")]
async fn hosted_transport_uses_only_scoped_access_headers_and_never_reuses_attempts() {
    let (_root, _store, provider, listener) = setup().await;
    let peer = tokio::spawn(async move {
        let (mut socket, _) = time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let request = time::timeout(Duration::from_secs(10), mock_request(&mut socket))
            .await
            .unwrap();
        let (headers, body) = request.split_once("\r\n\r\n").unwrap();
        let headers = headers.to_lowercase();
        assert!(headers.starts_with("post /backend-api/codex/responses http/1.1"));
        for expected in [
            "originator: morons",
            "chatgpt-account-id: web-account-fixture",
            "authorization: bearer ",
            "session-id:",
            "thread-id:",
            "x-client-request-id:",
        ] {
            assert!(headers.contains(expected));
        }
        for absent in ["web-refresh-fixture", "x-opencode", "cookie:"] {
            assert!(!headers.contains(absent));
        }
        let value: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(value["model"], super::super::MODEL);
        assert_eq!(value["tools"][0]["type"], "web_search");
        let body = super::super::tests::response_fixture();
        // Fixed native route legitimately omits media type; decoder still requires complete SSE.
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        socket.write_all(&body).await.unwrap();
        listener
    });
    let mut attempt = provider
        .new_attempt([1; 16], 1, "PRIVATE query", DataUseRestrictions::default())
        .unwrap();
    let (_, mut cancel) = provider_cancellation();
    assert!(!format!("{attempt:?}").contains("PRIVATE"));
    assert_ne!(attempt.identifier, attempt.request_id);
    let result = provider
        .prepare(&mut attempt, DataUseRestrictions::default(), &mut cancel)
        .await
        .unwrap()
        .execute(DataUseRestrictions::default(), &mut cancel)
        .await
        .unwrap();
    assert_eq!(result.search_calls, 1);
    assert!(matches!(
        provider
            .prepare(&mut attempt, DataUseRestrictions::default(), &mut cancel)
            .await,
        Err(ProviderError::InvalidRequest)
    ));
    let listener = time::timeout(Duration::from_secs(10), peer)
        .await
        .unwrap()
        .unwrap();
    assert!(
        time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
}
#[tokio::test(flavor = "current_thread")]
async fn hosted_transport_denial_framing_and_bad_results_never_retry() {
    let (_root, _store, provider, listener) = setup().await;
    let replies=[
        b"HTTP/1.1 403 Forbidden\r\nContent-Length: 7\r\nConnection: close\r\n\r\nPRIVATE".to_vec(),
        b"HTTP/1.1 302 Found\r\nLocation: https://example.com/PRIVATE\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nPRIVATE".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 262145\r\nConnection: close\r\n\r\n".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Type: application/json\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
    ];
    for (i, reply) in replies.into_iter().enumerate() {
        let mut attempt = provider
            .new_attempt(
                [i as u8 + 2; 16],
                1,
                "query",
                DataUseRestrictions::default(),
            )
            .unwrap();
        let (_, mut cancel) = provider_cancellation();
        let client = async {
            provider
                .prepare(&mut attempt, DataUseRestrictions::default(), &mut cancel)
                .await
                .unwrap()
                .execute(DataUseRestrictions::default(), &mut cancel)
                .await
        };
        let server = async {
            let (mut socket, _) = time::timeout(Duration::from_secs(10), listener.accept())
                .await
                .unwrap()
                .unwrap();
            time::timeout(Duration::from_secs(10), mock_request(&mut socket))
                .await
                .unwrap();
            socket.write_all(&reply).await.unwrap();
        };
        let (result, ()) = time::timeout(Duration::from_secs(10), async {
            tokio::join!(client, server)
        })
        .await
        .unwrap();
        let error = result.unwrap_err();
        assert!(matches!(
            (i, &error),
            (0, ProviderError::AuthenticationOrEntitlement)
                | (1, ProviderError::RedirectDenied)
                | (2, ProviderError::UnexpectedContentType)
                | (
                    3,
                    ProviderError::MalformedResponse | ProviderError::IncompleteResponse
                )
                | (4, ProviderError::ResponseLimitExceeded)
                | (5, ProviderError::MalformedResponse)
        ));
        let failure = attempt.failure(error);
        assert_eq!(
            failure.stage,
            [
                WebStage::HttpStatus,
                WebStage::HttpStatus,
                WebStage::ContentType,
                WebStage::Sse,
                WebStage::BodyBounds,
                WebStage::Headers
            ][i]
        );
        assert!(!format!("{error:?} {error} {failure:?}").contains("PRIVATE"));
        assert!(matches!(
            provider
                .prepare(&mut attempt, DataUseRestrictions::default(), &mut cancel)
                .await,
            Err(ProviderError::InvalidRequest)
        ));
        assert!(
            time::timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
    }
}
#[tokio::test(flavor = "current_thread")]
async fn hosted_transport_checks_instance_generation_policy_and_cancellation_before_network() {
    let (_root, store, provider, listener) = setup().await;
    let (_, mut cancel) = provider_cancellation();
    let mut foreign = SearchProvider::new(Arc::new(OpenAiCredentialProvider::new(store.clone())))
        .new_attempt([1; 16], 1, "query", DataUseRestrictions::default())
        .unwrap();
    assert!(matches!(
        provider
            .prepare(&mut foreign, DataUseRestrictions::default(), &mut cancel)
            .await,
        Err(ProviderError::InvalidRequest)
    ));
    let mut attempt = provider
        .new_attempt([1; 16], 1, "query", DataUseRestrictions::default())
        .unwrap();
    let blocked = DataUseRestrictions {
        block_training_use: true,
        require_zero_retention: false,
    };
    assert!(matches!(
        provider.prepare(&mut attempt, blocked, &mut cancel).await,
        Err(ProviderError::DataUseRestricted)
    ));
    let prepared = provider
        .prepare(&mut attempt, DataUseRestrictions::default(), &mut cancel)
        .await
        .unwrap();
    assert!(matches!(
        prepared.execute(blocked, &mut cancel).await,
        Err(ProviderError::DataUseRestricted)
    ));
    let (handle, mut cancelled) = provider_cancellation();
    handle.cancel();
    assert!(matches!(
        provider
            .prepare(&mut attempt, DataUseRestrictions::default(), &mut cancelled)
            .await,
        Err(ProviderError::Cancelled)
    ));
    let expires = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    store
        .set_openai_credential(
            MutationRequestId::from_bytes([2; 16]),
            1,
            OAuthTokens::fixture("replacement-fixture", "replacement-refresh", expires),
        )
        .await
        .unwrap();
    assert!(matches!(
        provider
            .prepare(&mut attempt, DataUseRestrictions::default(), &mut cancel)
            .await,
        Err(ProviderError::CredentialGenerationChanged)
    ));
    assert!(
        time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
}
#[tokio::test(flavor = "current_thread")]
async fn hosted_transport_dropped_inflight_future_cannot_be_prepared_again() {
    let (_root, _store, provider, listener) = setup().await;
    let (ready_tx, ready_rx) = oneshot::channel();
    let peer = tokio::spawn(async move {
        let (mut socket, _) = time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .unwrap()
            .unwrap();
        time::timeout(Duration::from_secs(10), mock_request(&mut socket))
            .await
            .unwrap();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await
            .unwrap();
        ready_tx.send(()).unwrap();
        time::sleep(Duration::from_secs(2)).await;
    });
    let mut attempt = provider
        .new_attempt([8; 16], 1, "query", DataUseRestrictions::default())
        .unwrap();
    let (_, mut cancellation) = provider_cancellation();
    {
        let dispatch = provider
            .prepare(
                &mut attempt,
                DataUseRestrictions::default(),
                &mut cancellation,
            )
            .await
            .unwrap();
        let pending = dispatch.execute(DataUseRestrictions::default(), &mut cancellation);
        tokio::pin!(pending);
        tokio::select! {
            result = &mut pending => panic!("unexpected early result: {result:?}"),
            ready = time::timeout(Duration::from_secs(10), ready_rx) => { ready.unwrap().unwrap(); }
        }
    }
    assert!(attempt.spent);
    assert!(matches!(
        provider
            .prepare(
                &mut attempt,
                DataUseRestrictions::default(),
                &mut cancellation
            )
            .await,
        Err(ProviderError::InvalidRequest)
    ));
    peer.abort();
    let _ = peer.await;
}

#[tokio::test(flavor = "current_thread")]
async fn hosted_transport_cancel_and_deadline_poison_an_inflight_attempt() {
    for deadline in [false, true] {
        let (_root, _store, mut provider, listener) = setup().await;
        if deadline {
            provider.total_timeout = Duration::from_millis(100);
        }
        let (ready_tx, ready_rx) = oneshot::channel();
        let peer = tokio::spawn(async move {
            let (mut socket, _) = time::timeout(Duration::from_secs(10), listener.accept())
                .await
                .unwrap()
                .unwrap();
            time::timeout(Duration::from_secs(10), mock_request(&mut socket))
                .await
                .unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            ready_tx.send(()).unwrap();
            time::sleep(Duration::from_secs(2)).await;
        });
        let mut attempt = provider
            .new_attempt([9; 16], 1, "query", DataUseRestrictions::default())
            .unwrap();
        let (handle, mut cancel) = provider_cancellation();
        let request = async {
            provider
                .prepare(&mut attempt, DataUseRestrictions::default(), &mut cancel)
                .await
                .unwrap()
                .execute(DataUseRestrictions::default(), &mut cancel)
                .await
        };
        let trigger = async {
            time::timeout(Duration::from_secs(10), ready_rx)
                .await
                .unwrap()
                .unwrap();
            if !deadline {
                handle.cancel();
            }
        };
        let (result, ()) = time::timeout(Duration::from_secs(10), async {
            tokio::join!(request, trigger)
        })
        .await
        .unwrap();
        assert!(matches!(
            result,
            Err(ProviderError::Cancelled | ProviderError::TotalTimeout)
        ));
        let failure = attempt.failure(result.unwrap_err());
        // Peer header writes do not prove the client consumed them before cancellation/deadline.
        assert!(matches!(
            failure.stage,
            WebStage::Headers | WebStage::BodyFraming
        ));
        assert_eq!(
            failure.category,
            if deadline {
                crate::web_diagnostic::WebCategory::TotalTimeout
            } else {
                crate::web_diagnostic::WebCategory::Cancelled
            }
        );
        let (_, mut fresh) = provider_cancellation();
        assert!(matches!(
            provider
                .prepare(&mut attempt, DataUseRestrictions::default(), &mut fresh)
                .await,
            Err(ProviderError::InvalidRequest)
        ));
        peer.abort();
        let _ = peer.await;
    }
}
