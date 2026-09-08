use super::*;
use morons_cli::ApplicationClient;
use morons_protocol::{ApplicationEvent, NativeResponseFailure, TranscriptPageDirection};

async fn authenticated_client(
    app: Arc<ServerApplication>,
) -> (
    ApplicationClient<tokio::io::DuplexStream>,
    tokio::task::JoinHandle<()>,
) {
    let (mut client, mut server) = tokio::io::duplex(16 * 1024);
    let task = tokio::spawn(async move {
        let key = morons_protocol::AuthenticationKey::from_bytes(
            [0x42; morons_protocol::AUTHENTICATION_KEY_BYTES],
        );
        let epoch =
            morons_protocol::HostEpoch::from_bytes([0x43; morons_protocol::HOST_EPOCH_BYTES]);
        morons_protocol::authenticate_server(&mut server, &key, &epoch)
            .await
            .unwrap();
        assert_eq!(
            crate::handle_handshake(&mut server, "fixture")
                .await
                .unwrap(),
            crate::HandshakeOutcome::Accepted
        );
        crate::handle_local_owner_requests(&mut server, &app)
            .await
            .unwrap();
    });
    let key = morons_protocol::AuthenticationKey::from_bytes(
        [0x42; morons_protocol::AUTHENTICATION_KEY_BYTES],
    );
    let epoch = morons_protocol::HostEpoch::from_bytes([0x43; morons_protocol::HOST_EPOCH_BYTES]);
    morons_protocol::authenticate_client(&mut client, &key, &epoch)
        .await
        .unwrap();
    morons_cli::perform_handshake(&mut client, "fixture")
        .await
        .unwrap();
    (ApplicationClient::from_negotiated_connection(client), task)
}

#[tokio::test(flavor = "current_thread")]
async fn native_protocol_diagnostic_follows_committed_failure_over_authenticated_ipc_without_history_or_replay()
 {
    let root = TestRoot::new("native-diagnostic");
    let selected = TestRoot::new("native-diagnostic-selected");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    store
        .set_openai_credential(
            PersistenceMutationRequestId::from_bytes([0x91; 16]),
            0,
            synthetic_tokens(),
        )
        .await
        .unwrap();
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0x92; 16]),
            None,
            selected.path().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let peer = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = read_http_request(&mut stream).await;
        let body = provider_output_body("resp_PRIVATE", r#"{"id":"msg_PRIVATE","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"PRIVATE-provider-output","annotations":[]}]}"#)
            .replace("muse-spark-1.2", "gpt-5.5").replace("\"total_tokens\":11", "\"total_tokens\":12");
        write_provider_headers(&mut stream, body.len()).await;
        stream.write_all(body.as_bytes()).await.unwrap();
        stream.shutdown().await.unwrap();
        listener
    });
    let app = Arc::new(ServerApplication::from_native_store_for_test(store, &base));
    let session_id = SessionId::from_bytes(*session.id.as_bytes());
    let (mut client, subscriber_task) = authenticated_client(app.clone()).await;
    let page = client
        .list_session_transcript(session_id, None, TranscriptPageDirection::Older, 1)
        .await
        .unwrap();
    let mut subscriber = client
        .subscribe_to_session(session_id, page.event_cursor)
        .await
        .unwrap();
    let (mut control, control_task) = authenticated_client(app.clone()).await;
    let accepted = control
        .submit_session_input(
            MutationRequestId::from_bytes([0x93; 16]),
            session_id,
            "No tools needed".into(),
            ModelService::OpenAiChatGpt,
            "gpt-5.5".into(),
        )
        .await
        .unwrap();
    let mut saw_terminal = false;
    time::timeout(TERMINAL_RUN_TEST_TIMEOUT, async {
        loop {
            let before = subscriber.cursor();
            let event = subscriber.next_event().await.unwrap();
            assert!(!format!("{event:?}").contains("PRIVATE"));
            match event {
                ApplicationEvent::SessionRunChanged { run, .. } if run.state.is_terminal() => {
                    assert_eq!(run.id, accepted.run.id);
                    assert_eq!(run.state, RunState::Failed);
                    assert_eq!(
                        run.failure,
                        Some(morons_protocol::RunFailureKind::ProviderProtocol)
                    );
                    saw_terminal = true;
                }
                ApplicationEvent::SessionNativeResponseDiagnostic {
                    session_id: scope,
                    run_id,
                    reason,
                } => {
                    assert!(
                        saw_terminal,
                        "diagnostic must not overtake committed terminal evidence"
                    );
                    assert_eq!(scope, session_id);
                    assert_eq!(run_id, accepted.run.id);
                    assert_eq!(reason, NativeResponseFailure::Usage);
                    assert_eq!(
                        subscriber.cursor(),
                        before,
                        "diagnostic has no durable cursor"
                    );
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    let page = control
        .list_session_transcript(session_id, None, TranscriptPageDirection::Older, 1)
        .await
        .unwrap();
    assert_eq!(page.entries.len(), 1);
    let mut later = control
        .subscribe_to_session(session_id, page.event_cursor)
        .await
        .unwrap();
    assert!(
        time::timeout(Duration::from_millis(50), later.next_event())
            .await
            .is_err(),
        "no ephemeral diagnostic replay on a new subscription"
    );
    let listener = peer.await.unwrap();
    assert!(
        time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
    drop(subscriber);
    drop(later);
    subscriber_task.await.unwrap();
    control_task.await.unwrap();
    app.shutdown().await;
    drop(app);
    let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM session_entries WHERE text LIKE '%PRIVATE%'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM tool_calls", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(db);
    let _reopened = SessionStore::open_for_test(root.path()).unwrap();
}
