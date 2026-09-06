use super::*;
mod installation;
use crate::{
    persistence::{SessionStore, credential_tests::TestRoot},
    provider::openai_auth::{
        OAuthTokens,
        tests::{fields, login, mock_request, submit},
    },
};
use morons_protocol::{
    ApplicationRequest, ApplicationResponse, ClientMessage, ServerMessage, read_server_message,
    write_client_message,
};
use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::AsyncWriteExt as _,
    net::{TcpListener, TcpStream},
};

fn tokens() -> OAuthTokens {
    OAuthTokens::fixture(
        "login-fixture-account",
        "login-fixture-refresh",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600,
    )
}
fn setup() -> (TestRoot, Arc<SessionStore>, Arc<LoginSupervisor>) {
    let root = TestRoot::new("login-supervision");
    let store = Arc::new(SessionStore::open_for_test(root.path()).unwrap());
    let credentials = Arc::new(OpenAiCredentialProvider::new(store.clone()));
    let (shutdown, _) = watch::channel(false);
    (root, store, LoginSupervisor::new(credentials, shutdown))
}
async fn wait_for_drain(supervisor: &LoginSupervisor) {
    time::timeout(Duration::from_secs(3), async {
        while !supervisor.stopping.load(Ordering::Acquire) || supervisor.active.try_lock().is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn id() -> MutationRequestId {
    MutationRequestId::from_bytes([2; 16])
}
async fn start(
    supervisor: &Arc<LoginSupervisor>,
    provider: &TcpListener,
    generation: u64,
) -> LoginConnection {
    let uri = format!("http://{}/oauth/token", provider.local_addr().unwrap())
        .parse()
        .unwrap();
    supervisor
        .start_with(id(), generation, async {
            Ok(login(uri, &Arc::new(Semaphore::new(1)), Duration::from_secs(60)).await)
        })
        .await
        .unwrap()
}
fn callback(url: &OpenAiAuthorizationUrl) -> (std::net::SocketAddr, String) {
    let fields = fields(url.as_str().split_once('?').unwrap().1);
    let uri: http::Uri = fields["redirect_uri"].parse().unwrap();
    (
        format!("127.0.0.1:{}", uri.port_u16().unwrap())
            .parse()
            .unwrap(),
        fields["state"].clone(),
    )
}
async fn exchange(provider: &TcpListener) {
    let (mut stream, _) = provider.accept().await.unwrap();
    let _ = mock_request(&mut stream).await;
    let tokens = tokens();
    let (access, refresh, _, _) = tokens.stored_parts();
    let body = serde_json::to_vec(
        &serde_json::json!({"access_token":access,"refresh_token":refresh,"expires_in":3600}),
    )
    .unwrap();
    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
    stream.write_all(&body).await.unwrap();
}
#[tokio::test(flavor = "current_thread")]
async fn stale_generation_and_shutdown_do_not_poll_the_oauth_factory() {
    let (_root, _store, supervisor) = setup();
    let never = std::future::poll_fn(|_| panic!("must not bind a callback or contact any service"));
    assert!(matches!(
        supervisor.start_with(id(), 1, never).await,
        Err(ApplicationError::CredentialGenerationConflict)
    ));
    supervisor.shutdown().await;
    assert!(matches!(
        supervisor.start_with(id(), 0, std::future::pending()).await,
        Err(ApplicationError::ServiceUnavailable)
    ));
}
#[tokio::test(flavor = "current_thread")]
async fn login_is_nonqueued_and_exact_owner_cancellation_preserves_credentials() {
    let (root, store, supervisor) = setup();
    store
        .set_openai_credential(
            crate::persistence::MutationRequestId::from_bytes([1; 16]),
            0,
            tokens(),
        )
        .await
        .unwrap();
    let before = fs::read(root.path().join("credentials/openai-chatgpt.state")).unwrap();
    let provider = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut connection = start(&supervisor, &provider, 1).await;
    let (address, _) = callback(connection.url.as_ref().unwrap());
    assert!(matches!(
        supervisor.start_with(id(), 1, std::future::pending()).await,
        Err(ApplicationError::OpenAiLoginFailed {
            failure: Failure::Busy
        })
    ));
    assert!(
        connection
            .cancel(MutationRequestId::from_bytes([9; 16]))
            .is_err()
    );
    assert!(connection.outcome.borrow().is_none());
    connection.cancel(id()).unwrap();
    assert_eq!(
        connection.finish().await,
        Outcome::CancelledBeforeInstallation
    );
    assert!(TcpStream::connect(address).await.is_err());
    assert!(supervisor.active.lock().await.is_none());
    assert_eq!(
        fs::read(root.path().join("credentials/openai-chatgpt.state")).unwrap(),
        before
    );
    assert!(
        time::timeout(Duration::from_millis(30), provider.accept())
            .await
            .is_err()
    );
}
#[tokio::test(flavor = "current_thread")]
async fn login_stream_publishes_only_after_installation_and_accepts_no_other_scope() {
    let (root, store, supervisor) = setup();
    let provider = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let connection = start(&supervisor, &provider, 0).await;
    let (mut client, mut server) = tokio::io::duplex(8192);
    let task = tokio::spawn(async move {
        crate::connection::stream_openai_login(&mut server, 7, connection)
            .await
            .unwrap();
    });
    let Some(ServerMessage::Response {
        request_id: 7,
        response: ApplicationResponse::OpenAiLoginStarted { attempt_id, url },
    }) = read_server_message(&mut client).await.unwrap()
    else {
        panic!("start response expected")
    };
    assert_eq!(attempt_id, id());
    let (address, state) = callback(&url);
    drop(url);
    let ((), response) = tokio::join!(
        exchange(&provider),
        submit(address, state, "synthetic-code")
    );
    assert!(response.starts_with(b"HTTP/1.1 200"));
    assert_eq!(
        read_server_message(&mut client).await.unwrap(),
        Some(ServerMessage::OpenAiLoginFinished {
            attempt_id: id(),
            outcome: Outcome::Installed { generation: 1 }
        })
    );
    assert_eq!(
        store.openai_credential_status().await.unwrap().generation,
        1
    );
    assert!(
        root.path()
            .join("credentials/openai-chatgpt.state")
            .exists()
    );
    task.await.unwrap();
    assert!(TcpStream::connect(address).await.is_err());
    assert!(
        time::timeout(Duration::from_millis(30), provider.accept())
            .await
            .is_err()
    );
}
#[tokio::test(flavor = "current_thread")]
async fn disconnected_slow_and_invalid_login_consumers_cancel_without_detaching() {
    for behavior in ["disconnect", "slow", "wrong-id", "cancel"] {
        let (_root, store, supervisor) = setup();
        let provider = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let connection = start(&supervisor, &provider, 0).await;
        let (address, _) = callback(connection.url.as_ref().unwrap());
        let (mut client, mut server) = tokio::io::duplex(if behavior == "slow" { 1 } else { 8192 });
        let task = tokio::spawn(async move {
            crate::connection::stream_openai_login(&mut server, 1, connection).await
        });
        if behavior != "slow" {
            let _ = read_server_message(&mut client).await.unwrap();
            if behavior == "cancel" || behavior == "wrong-id" {
                write_client_message(
                    &mut client,
                    &ClientMessage::request(
                        2,
                        ApplicationRequest::CancelOpenAiLogin {
                            attempt_id: if behavior == "cancel" {
                                id()
                            } else {
                                MutationRequestId::from_bytes([8; 16])
                            },
                        },
                    ),
                )
                .await
                .unwrap();
                if behavior == "cancel" {
                    assert_eq!(
                        read_server_message(&mut client).await.unwrap(),
                        Some(ServerMessage::OpenAiLoginFinished {
                            attempt_id: id(),
                            outcome: Outcome::CancelledBeforeInstallation
                        })
                    );
                }
            }
        }
        if behavior == "disconnect" {
            drop(client);
        }
        let result = time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            result.is_ok(),
            behavior == "cancel" || behavior == "disconnect"
        );
        supervisor.shutdown().await;
        assert!(supervisor.active.lock().await.is_none());
        assert!(TcpStream::connect(address).await.is_err());
        assert_eq!(
            store.openai_credential_status().await.unwrap().generation,
            0
        );
    }
}
#[tokio::test(flavor = "current_thread")]
async fn cancellation_during_installation_reports_the_committed_effect_not_rollback() {
    let (_root, store, supervisor) = setup();
    store
        .set_openai_credential(
            crate::persistence::MutationRequestId::from_bytes([1; 16]),
            0,
            tokens(),
        )
        .await
        .unwrap();
    let lease = store.begin_openai_access(1).await.unwrap();
    let provider = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut connection = start(&supervisor, &provider, 1).await;
    let (address, state) = callback(connection.url.as_ref().unwrap());
    let _ = tokio::join!(
        exchange(&provider),
        submit(address, state, "synthetic-code")
    );
    supervisor.install_started.notified().await;
    connection.cancel(id()).unwrap();
    assert!(connection.outcome.borrow().is_none());
    drop(lease);
    assert_eq!(
        connection.finish().await,
        Outcome::Installed { generation: 2 }
    );
    assert_eq!(
        store.openai_credential_status().await.unwrap().generation,
        2
    );
}
#[tokio::test(flavor = "current_thread")]
async fn abandoned_drain_keeps_registration_until_the_install_task_is_joined() {
    let (_root, store, supervisor) = setup();
    store
        .set_openai_credential(
            crate::persistence::MutationRequestId::from_bytes([1; 16]),
            0,
            tokens(),
        )
        .await
        .unwrap();
    let lease = store.begin_openai_access(1).await.unwrap();
    let provider = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut connection = start(&supervisor, &provider, 1).await;
    let (address, state) = callback(connection.url.as_ref().unwrap());
    let _ = tokio::join!(
        exchange(&provider),
        submit(address, state, "synthetic-code")
    );
    supervisor.install_started.notified().await;
    let drain = tokio::spawn({
        let s = supervisor.clone();
        async move { s.shutdown().await }
    });
    wait_for_drain(&supervisor).await;
    drain.abort();
    let _ = drain.await;
    assert!(supervisor.active.lock().await.is_some());
    assert!(connection.outcome.borrow().is_none());
    drop(lease);
    supervisor.shutdown().await;
    assert_eq!(
        connection.finish().await,
        Outcome::Installed { generation: 2 }
    );
    assert!(supervisor.active.lock().await.is_none());
}
#[tokio::test(flavor = "current_thread")]
async fn shutdown_during_admission_drops_the_unpublished_callback() {
    let (_root, _store, supervisor) = setup();
    let provider = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let login = login(
        format!("http://{}/oauth/token", provider.local_addr().unwrap())
            .parse()
            .unwrap(),
        &Arc::new(Semaphore::new(1)),
        Duration::from_secs(60),
    )
    .await;
    let (sent, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = tokio::sync::oneshot::channel();
    let begin = tokio::spawn({
        let s = supervisor.clone();
        async move {
            s.start_with(id(), 0, async {
                sent.send(()).unwrap();
                wait.await.unwrap();
                Ok(login)
            })
            .await
        }
    });
    ready.await.unwrap();
    let drain = tokio::spawn({
        let s = supervisor.clone();
        async move { s.shutdown().await }
    });
    wait_for_drain(&supervisor).await;
    release.send(()).unwrap();
    assert!(matches!(
        begin.await.unwrap(),
        Err(ApplicationError::ServiceUnavailable)
    ));
    drain.await.unwrap();
    assert!(supervisor.active.lock().await.is_none());
}
