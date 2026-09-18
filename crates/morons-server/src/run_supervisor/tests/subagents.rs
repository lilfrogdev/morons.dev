use super::*;

#[tokio::test(flavor = "current_thread")]
async fn unsolicited_task_never_dispatches_children_even_with_a_stored_setting() {
    rejected_task(None).await;
}

pub(super) async fn rejected_task(native_model: Option<&'static str>) {
    let root = TestRoot::new("retired-task");
    let selected = TestRoot::new("retired-task-selected");
    fs::write(selected.path().join("sample.txt"), "preserved").unwrap();
    let store = SessionStore::open_for_test(root.path()).unwrap();
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0x74; 16]),
            0,
            b"synthetic-test-key".to_vec(),
        )
        .await
        .unwrap();
    store
        .set_subagent_model_setting(
            PersistenceMutationRequestId::from_bytes([0x73; 16]),
            crate::persistence::SubagentModelSetting::Explicit {
                service: RunService::Go,
                model_id: "glm-5.3-flash".into(),
            },
        )
        .await
        .unwrap();
    if native_model.is_some() {
        store
            .set_openai_credential(
                PersistenceMutationRequestId::from_bytes([0x72; 16]),
                0,
                native::synthetic_tokens(),
            )
            .await
            .unwrap();
    }
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0x75; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = time::timeout(TERMINAL_RUN_TEST_TIMEOUT, listener.accept())
            .await
            .unwrap()
            .unwrap();
        let request = String::from_utf8(read_http_request(&mut stream).await).unwrap();
        assert!(request.starts_with(if native_model.is_some() {
            "POST /backend-api/codex/responses"
        } else {
            "POST /zen/v1/responses"
        }));
        let body = request_body(&request);
        assert_eq!(body["model"], native_model.unwrap_or("muse-spark-1.2"));
        assert!(
            body["tools"]
                .as_array()
                .unwrap()
                .iter()
                .all(|tool| tool["name"] != "task")
        );
        let output = serde_json::json!({
            "id": "fc_task", "type": "function_call", "status": "completed",
            "call_id": "task_call", "name": "task",
            "arguments": serde_json::json!({"context":"Check", "tasks":[{"task":"Overwrite sample.txt"}]}).to_string()
        });
        if let Some(model) = native_model {
            native::write_native_model(&mut stream, "resp_task", &output.to_string(), model).await;
        } else {
            write_provider_output(&mut stream, "resp_task", &output.to_string()).await;
        }
        listener
    });
    let application = ServerApplication::from_native_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.id.as_bytes());
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        application
            .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
                mutation_request_id: MutationRequestId::from_bytes([0x76; 16]),
                session_id,
                text: "Check the file".into(),
                attachments: Vec::new(),
                service: if native_model.is_some() {
                    ModelService::OpenAiChatGpt
                } else {
                    ModelService::Zen
                },
                model_id: native_model.unwrap_or("muse-spark-1.2").into(),
            })
            .await
            .unwrap()
    else {
        panic!("expected accepted input")
    };
    assert_eq!(
        wait_for_terminal(&application, session_id, run.id).await,
        RunState::Failed
    );
    let listener = server.await.unwrap();
    assert!(
        time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
    application.shutdown().await;
    let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    for table in [
        "tool_calls",
        "task_model_bindings",
        "child_runs",
        "child_journal",
    ] {
        assert_eq!(
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0,
            "{table}"
        );
    }
    assert_eq!(
        fs::read_to_string(selected.path().join("sample.txt")).unwrap(),
        "preserved"
    );
    drop(db);
    SessionStore::open_for_test(root.path()).unwrap();
}
