use super::*;
use crate::persistence::maintenance::MaintenanceState;

fn selection() -> RunModelSelection {
    RunModelSelection {
        service: RunService::OpenAiChatGpt,
        model_id: "gpt-5.5".into(),
        protocol_revision: 5,
        maximum_input_tokens: 96_000,
        maximum_output_tokens: 32_000,
        supports_tool_calls: true,
        supports_image_input: true,
    }
}
async fn fixture(
    background: bool,
) -> (
    TestRoot,
    TestRoot,
    Arc<SessionStore>,
    crate::persistence::SessionId,
    crate::persistence::RunId,
) {
    let root = TestRoot::new("native-compaction");
    let selected = TestRoot::new("native-compaction-selected");
    let store = Arc::new(
        if background {
            SessionStore::open_for_test_with_maintenance(root.path())
        } else {
            SessionStore::open_for_test(root.path())
        }
        .unwrap(),
    );
    store
        .set_openai_credential(
            PersistenceMutationRequestId::from_bytes([0xa1; 16]),
            0,
            synthetic_tokens(),
        )
        .await
        .unwrap();
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0xa2; 16]),
            None,
            selected.path().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    let mut last = None;
    for index in 1..=5 {
        last = Some(
            append_completed_model_run(
                &store,
                session.id,
                index,
                &format!("NATIVE_SOURCE_{index}"),
                if background { 11_000 } else { 100 },
                selection(),
            )
            .await,
        );
    }
    (root, selected, store, session.id, last.unwrap())
}
fn output(text: &str) -> String {
    serde_json::json!({"id":"msg_compact","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":text,"annotations":[]}]}).to_string()
}

#[tokio::test(flavor = "current_thread")]
async fn native_foreground_compaction_uses_an_independent_tools_free_turn() {
    let (root, _selected, store, session, _) = fixture(false).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut first = None;
        for step in 0..2 {
            let (mut stream, _) = time::timeout(TERMINAL_RUN_TEST_TIMEOUT, listener.accept())
                .await
                .unwrap()
                .unwrap();
            let request = String::from_utf8(read_http_request(&mut stream).await).unwrap();
            assert!(request.starts_with("POST /backend-api/codex/responses"));
            let session = request_header(&request, "session-id");
            let body: serde_json::Value =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert!(body.get("max_output_tokens").is_none());
            if step == 0 {
                assert_eq!(body["tools"], serde_json::json!([]));
                first = Some(session);
            } else {
                assert_ne!(first.as_ref().unwrap(), &session);
                assert!(
                    body["input"]
                        .to_string()
                        .contains("NATIVE_COMPACTION_SUMMARY")
                );
            }
            write_native(
                &mut stream,
                &format!("resp_compact_{step}"),
                &output(if step == 0 {
                    "NATIVE_COMPACTION_SUMMARY"
                } else {
                    "Compacted."
                }),
            )
            .await;
        }
    });
    let app = ServerApplication::from_native_shared_for_test(store.clone(), &base);
    let session_id = SessionId::from_bytes(*session.as_bytes());
    let response = app
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0xa3; 16]),
            session_id,
            text: "/compact preserve the decision".into(),
            attachments: Vec::new(),
            service: ModelService::OpenAiChatGpt,
            model_id: "gpt-5.5".into(),
        })
        .await
        .unwrap();
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        response
    else {
        panic!("expected run")
    };
    assert_eq!(
        wait_for_terminal(&app, session_id, run.id).await,
        RunState::Succeeded
    );
    server.await.unwrap();
    app.shutdown().await;
    drop(app);
    drop(store);
    let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT open_code_service FROM context_checkpoints",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        3
    );
    drop(db);
    let _store = SessionStore::open_for_test(root.path()).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn native_background_compaction_pins_its_provider_and_ignores_unrelated_credential_changes() {
    let (root, _selected, store, session, trigger) = fixture(true).await;
    let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    db.execute("UPDATE provider_operation_facts SET input_tokens=60000,total_tokens=60010 WHERE fact_kind=3",[]).unwrap();
    drop(db);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let providers = crate::provider::dispatch::ModelProviders::for_test(store.clone(), &base);
    let supervisor = crate::maintenance_supervisor::MaintenanceSupervisor::new(
        store.clone(),
        providers,
        tokio::sync::watch::channel(false).0,
    );
    let server = tokio::spawn(async move {
        let (mut stream, _) = time::timeout(TERMINAL_RUN_TEST_TIMEOUT, listener.accept())
            .await
            .unwrap()
            .unwrap();
        let request = String::from_utf8(read_http_request(&mut stream).await).unwrap();
        assert!(request.starts_with("POST /backend-api/codex/responses"));
        let body: serde_json::Value =
            serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["tools"], serde_json::json!([]));
        write_native(
            &mut stream,
            "resp_background",
            &output("NATIVE_BACKGROUND_SUMMARY"),
        )
        .await;
    });
    supervisor.maybe_start(session, trigger).await;
    time::timeout(TERMINAL_RUN_TEST_TIMEOUT, async {
        loop {
            let status = store
                .session_context_status(session, selection())
                .await
                .unwrap();
            if status
                .background_compaction
                .latest
                .is_some_and(|job| job.state == MaintenanceState::Ready)
            {
                break;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    server.await.unwrap();
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0xa3; 16]),
            0,
            b"unrelated-synthetic-opencode".to_vec(),
        )
        .await
        .unwrap();
    let run = store
        .accept_session_input(
            PersistenceMutationRequestId::from_bytes([0xa4; 16]),
            session,
            "continue".into(),
            selection(),
        )
        .await
        .unwrap()
        .run;
    store.activate_run(run.id).await.unwrap();
    assert!(store.maintenance_boundary(run.id).await.unwrap().is_none());
    let context = store.load_run_context(run.id).await.unwrap();
    assert_eq!(
        context.checkpoint.unwrap().summary,
        "NATIVE_BACKGROUND_SUMMARY"
    );
    assert!(context.compaction_plan.is_none());
    store.finish_run_stopped(run.id, None).await.unwrap();
    supervisor.shutdown().await;
    drop(supervisor);
    drop(store);
    let _store = SessionStore::open_for_test(root.path()).unwrap();
}
