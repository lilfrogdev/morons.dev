use super::*;
use crate::provider::{
    openai_auth::{OpenAiCredentialProvider, tests::mock_request},
    provider_cancellation,
};
use tokio::{
    io::AsyncReadExt as _,
    net::TcpListener,
    sync::oneshot,
    time::{self, Duration},
};

#[tokio::test(flavor = "current_thread")]
async fn abandoning_refresh_future_closes_the_request_and_never_replays() {
    let (_root, store) = configured("aborted-refresh").await;
    let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let source = Arc::new(OpenAiCredentialProvider::for_test(
        store.clone(),
        format!("http://{}/oauth/token", server.local_addr().unwrap())
            .parse()
            .unwrap(),
    ));
    let (sent, received) = oneshot::channel();
    let request = tokio::spawn(async move {
        let (mut stream, _) = server.accept().await.unwrap();
        let _ = mock_request(&mut stream).await;
        sent.send(()).unwrap();
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
        server
    });
    let task = tokio::spawn(async move {
        let (_, mut cancel) = provider_cancellation();
        let _lease = source.lease(1, &mut cancel).await.unwrap();
    });
    received.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let server = time::timeout(Duration::from_secs(2), request)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        store.begin_openai_access(1).await,
        Err(PersistenceError::CredentialReauthenticationRequired)
    ));
    assert!(
        time::timeout(Duration::from_millis(30), server.accept())
            .await
            .is_err()
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn failed_file_write_poisoning_requires_restart_before_reuse() {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return;
    }
    let (root, store) = configured("credential-write-failure").await;
    drop(store);
    let mut file = OpenAiCredentialStore::open(root.path()).unwrap();
    let path = root.path().join("credentials/openai-chatgpt.state");
    let before = fs::read(&path).unwrap();
    let directory = root.path().join("credentials");
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();
    let result = file.apply(1, [9; 16], None, now());
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    assert!(!file.is_consistent());
    assert!(file.ensure_consistent().is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    let restarted = OpenAiCredentialStore::open(root.path()).unwrap();
    restarted.ensure_consistent().unwrap();
    assert_eq!(restarted.identity().generation, 1);
}

#[test]
fn credential_state_rejects_inconsistent_phase_revision_and_identity() {
    for (phase, revision, mutation, refresh, tokens_present) in [
        (Phase::Active, 0, [1; 16], [0; 16], true),
        (Phase::Active, 1, [0; 16], [0; 16], true),
        (Phase::Active, 1, [1; 16], [2; 16], true),
        (Phase::Active, 2, [1; 16], [0; 16], true),
        (Phase::Dispatched, 1, [1; 16], [0; 16], true),
        (Phase::Removed, 1, [1; 16], [0; 16], false),
        (Phase::Removed, 0, [1; 16], [0; 16], true),
    ] {
        let state = State {
            generation: 1,
            phase,
            revision,
            mutation,
            refresh,
            tokens: tokens_present.then(tokens),
        };
        assert!(format::encode(&state).is_err());
    }
}
