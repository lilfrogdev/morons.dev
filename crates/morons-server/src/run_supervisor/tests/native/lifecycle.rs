use super::*;

async fn fixture() -> (
    TestRoot,
    TestRoot,
    Arc<SessionStore>,
    crate::persistence::SessionId,
) {
    let root = TestRoot::new("native-lifecycle");
    let selected = TestRoot::new("native-lifecycle-selected");
    fs::write(selected.path().join("file.txt"), "preserved").unwrap();
    let store = Arc::new(SessionStore::open_for_test(root.path()).unwrap());
    store
        .set_openai_credential(
            PersistenceMutationRequestId::from_bytes([0xf1; 16]),
            0,
            synthetic_tokens(),
        )
        .await
        .unwrap();
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0xf2; 16]),
            None,
            selected.path().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    (root, selected, store, session.id)
}
async fn start(
    app: &ServerApplication,
    session: crate::persistence::SessionId,
) -> (SessionId, RunId) {
    let session_id = SessionId::from_bytes(*session.as_bytes());
    let result = app
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0xf3; 16]),
            session_id,
            text: "Read file.txt".into(),
            attachments: Vec::new(),
            service: ModelService::OpenAiChatGpt,
            model_id: "gpt-5.5".into(),
        })
        .await
        .unwrap();
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        result
    else {
        panic!("expected run")
    };
    (session_id, run.id)
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_during_native_credential_wait_is_cancelled_without_dispatch_or_replay() {
    let (root, _selected, store, session) = fixture().await;
    let lease = store.begin_openai_access(1).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = ServerApplication::from_native_shared_for_test(store.clone(), &base);
    let (session_id, run_id) = start(&app, session).await;
    let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    time::timeout(TERMINAL_RUN_TEST_TIMEOUT, async {
        loop {
            if db
                .query_row(
                    "SELECT COUNT(*) FROM provider_operation_facts WHERE fact_kind=1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap()
                == 1
            {
                break;
            }
            time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    app.execute_for_local_owner(ApplicationRequest::CancelRun {
        mutation_request_id: MutationRequestId::from_bytes([0xf4; 16]),
        session_id,
        run_id,
    })
    .await
    .unwrap();
    assert_eq!(
        wait_for_terminal(&app, session_id, run_id).await,
        RunState::Cancelled
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM provider_operation_facts WHERE fact_kind=2",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert!(
        time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
    drop(lease);
    drop(db);
    app.shutdown().await;
    drop(app);
    drop(store);
    let _store = SessionStore::open_for_test(root.path()).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn native_account_replacement_does_not_rebind_an_accepted_run() {
    let (root, _selected, store, session) = fixture().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (seen, seen_rx) = oneshot::channel();
    let (resume, resume_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_request(&mut stream).await;
        let output=serde_json::json!({"id":"fc_change","type":"function_call","status":"completed","call_id":"change_read","name":"read","arguments":serde_json::json!({"path":"file.txt"}).to_string()}).to_string();
        let body =
            provider_output_body("resp_change", &output).replace("muse-spark-1.2", "gpt-5.5");
        write_provider_headers(&mut stream, body.len()).await;
        seen.send(()).unwrap();
        resume_rx.await.unwrap();
        stream.write_all(body.as_bytes()).await.unwrap();
        stream.shutdown().await.unwrap();
        assert!(
            time::timeout(Duration::from_millis(500), listener.accept())
                .await
                .is_err(),
            "a stale run must not send another request"
        );
    });
    let app = ServerApplication::from_native_shared_for_test(store.clone(), &base);
    let (session_id, run_id) = start(&app, session).await;
    time::timeout(TERMINAL_RUN_TEST_TIMEOUT, seen_rx)
        .await
        .unwrap()
        .unwrap();
    store
        .set_openai_credential(
            PersistenceMutationRequestId::from_bytes([0xf4; 16]),
            1,
            synthetic_tokens(),
        )
        .await
        .unwrap();
    resume.send(()).unwrap();
    assert_eq!(
        wait_for_terminal(&app, session_id, run_id).await,
        RunState::Failed
    );
    server.await.unwrap();
    app.shutdown().await;
    drop(app);
    drop(store);
    let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT credential_generation,failure_kind FROM runs",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        )
        .unwrap(),
        (1, 1)
    );
    drop(db);
    let _store = SessionStore::open_for_test(root.path()).unwrap();
}
