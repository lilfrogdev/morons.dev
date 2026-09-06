use super::*;
use crate::provider::{
    openai_auth::{OpenAiCredentialError, OpenAiCredentialProvider, tests::mock_request},
    provider_cancellation,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    sync::oneshot,
    time::{self, Duration},
};

async fn reply(stream: &mut tokio::net::TcpStream, account: &str) {
    let tokens = OAuthTokens::fixture(account, "rotated-refresh", now() + 3600);
    let (access, refresh, _, _) = tokens.stored_parts();
    let body=serde_json::to_vec(&serde_json::json!({"access_token":access,"refresh_token":refresh,"expires_in":3600,"token_type":"Bearer"})).unwrap();
    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
    stream.write_all(&body).await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn refresh_is_durable_single_flight_account_scoped_and_independent_of_opencode() {
    let (root, store) = configured("oauth-refresh").await;
    let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let source = Arc::new(OpenAiCredentialProvider::for_test(
        store.clone(),
        format!("http://{}/oauth/token", server.local_addr().unwrap())
            .parse()
            .unwrap(),
    ));
    let (sent_tx, sent_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let server_task = tokio::spawn(async move {
        let (mut socket, _) = server.accept().await.unwrap();
        let request = mock_request(&mut socket).await;
        assert!(request.starts_with("POST /oauth/token HTTP/1.1"));
        assert!(!request.to_lowercase().contains("authorization:"));
        let body = request.split_once("\r\n\r\n").unwrap().1;
        assert!(body.contains("grant_type=refresh_token"));
        assert!(body.contains("refresh_token=refresh-fixture"));
        assert_eq!(body.split('&').count(), 3);
        sent_tx.send(()).unwrap();
        release_rx.await.unwrap();
        reply(&mut socket, "account-fixture").await;
        server
    });
    let first_source = source.clone();
    let (ready_tx, ready_rx) = oneshot::channel();
    let (drop_tx, drop_rx) = oneshot::channel();
    let first = tokio::spawn(async move {
        let (_, mut cancel) = provider_cancellation();
        let lease = first_source.lease(1, &mut cancel).await.unwrap();
        let headers = lease.authorization_headers();
        assert_eq!(headers["chatgpt-account-id"], "account-fixture");
        assert!(headers["authorization"].is_sensitive());
        assert!(!format!("{lease:?}").contains("account-fixture"));
        ready_tx.send(()).unwrap();
        drop_rx.await.unwrap();
        drop(lease);
    });
    sent_rx.await.unwrap();
    let path = root.path().join("credentials/openai-chatgpt.state");
    let state = format::decode(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(state.phase, Phase::Dispatched);
    assert_eq!(state.revision, 1);
    assert_eq!(
        store.openai_credential_status().await.unwrap().state,
        OpenAiCredentialState::Configured
    );
    assert_eq!(
        format::decode(&fs::read(&path).unwrap()).unwrap().phase,
        Phase::Dispatched
    );
    store
        .set_open_code_credential(
            MutationRequestId::from_bytes([2; 16]),
            0,
            b"other-provider".to_vec(),
        )
        .await
        .unwrap();
    let second_source = source.clone();
    let second = tokio::spawn(async move {
        let (_, mut cancel) = provider_cancellation();
        let lease = second_source.lease(1, &mut cancel).await.unwrap();
        assert_eq!(
            lease.authorization_headers()["chatgpt-account-id"],
            "account-fixture"
        );
    });
    assert!(
        time::timeout(Duration::from_millis(30), async {
            while !second.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_err()
    );
    release_tx.send(()).unwrap();
    ready_rx.await.unwrap();
    let state = format::decode(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(state.phase, Phase::Active);
    assert_eq!(state.revision, 2);
    assert_eq!(state.generation, 1);
    assert_eq!(state.mutation, [1; 16]);
    assert!(!second.is_finished());
    drop_tx.send(()).unwrap();
    first.await.unwrap();
    second.await.unwrap();
    let server = server_task.await.unwrap();
    assert!(
        time::timeout(Duration::from_millis(30), server.accept())
            .await
            .is_err()
    );
    drop(source);
    drop(store);
    let reopened = SessionStore::open_for_test(root.path()).unwrap();
    assert_eq!(
        reopened
            .openai_credential_status()
            .await
            .unwrap()
            .generation,
        1
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_or_rejected_refresh_requires_login_and_never_replays() {
    for outcome in ["cancel", "account", "reject", "invalid"] {
        let (root, store) = configured(outcome).await;
        let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source = OpenAiCredentialProvider::for_test(
            store.clone(),
            format!("http://{}/oauth/token", server.local_addr().unwrap())
                .parse()
                .unwrap(),
        );
        let (handle, mut cancel) = provider_cancellation();
        let request = source.lease(1, &mut cancel);
        let exchange = async {
            let (mut stream, _) = server.accept().await.unwrap();
            let _ = mock_request(&mut stream).await;
            match outcome {
                "cancel"=>{handle.cancel();let mut byte=[0];assert_eq!(stream.read(&mut byte).await.unwrap(),0);}
                "account"=>reply(&mut stream,"wrong-account").await,
                "reject"=>stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n").await.unwrap(),
                _=>stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}").await.unwrap(),
            }
        };
        let (result, ()) = tokio::join!(request, exchange);
        assert!(result.is_err());
        drop(result);
        let status = store.openai_credential_status().await.unwrap();
        assert_eq!(
            status.state,
            OpenAiCredentialState::ReauthenticationRequired
        );
        assert_eq!(status.generation, 1);
        let (_, mut cancel) = provider_cancellation();
        assert!(matches!(
            source.lease(1, &mut cancel).await,
            Err(OpenAiCredentialError::Persistence(
                PersistenceError::CredentialReauthenticationRequired
            ))
        ));
        assert!(
            time::timeout(Duration::from_millis(30), server.accept())
                .await
                .is_err()
        );
        drop(source);
        drop(store);
        let reopened = SessionStore::open_for_test(root.path()).unwrap();
        assert_eq!(
            reopened.openai_credential_status().await.unwrap().state,
            OpenAiCredentialState::ReauthenticationRequired
        );
        drop(reopened);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn abandoned_refresh_ownership_is_disabled_on_next_exclusive_access() {
    let (_root, store) = configured("abandoned").await;
    let lease = store.begin_openai_access(1).await.unwrap();
    drop(lease);
    assert_eq!(
        store.openai_credential_status().await.unwrap().state,
        OpenAiCredentialState::Configured
    );
    assert!(matches!(
        store.begin_openai_access(1).await,
        Err(PersistenceError::CredentialReauthenticationRequired)
    ));
    assert_eq!(
        store.openai_credential_status().await.unwrap().state,
        OpenAiCredentialState::ReauthenticationRequired
    );
}
