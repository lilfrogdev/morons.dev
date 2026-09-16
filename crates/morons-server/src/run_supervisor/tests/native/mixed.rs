use super::*;

#[tokio::test(flavor = "current_thread")]
async fn mixed_provider_children_pin_their_own_generations_and_ignore_later_model_changes() {
    for model in ["gpt-5.5", "gpt-5.6-luna", "gpt-daybreak-blue-latest"] {
        mixed_child_flow(Some(model), None).await;
        mixed_child_flow(None, Some(model)).await;
    }
    mixed_child_flow(Some("gpt-6-astra"), Some("gpt-5.6-terra")).await;
}

async fn mixed_child_flow(parent_native: Option<&'static str>, child_native: Option<&'static str>) {
    let native_parent = parent_native.is_some();
    let root = TestRoot::new("mixed-provider");
    let selected = TestRoot::new("mixed-provider-selected");
    fs::write(selected.path().join("child.txt"), "child source").unwrap();
    let store = SessionStore::open_for_test(root.path()).unwrap();
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0xb1; 16]),
            0,
            b"synthetic-opencode-mixed".to_vec(),
        )
        .await
        .unwrap();
    for generation in 0..3 {
        store
            .set_openai_credential(
                PersistenceMutationRequestId::from_bytes(
                    [0xb2 + u8::try_from(generation).unwrap(); 16],
                ),
                generation,
                synthetic_tokens(),
            )
            .await
            .unwrap();
    }
    let child_service = if child_native.is_some() {
        crate::persistence::RunService::OpenAiChatGpt
    } else {
        crate::persistence::RunService::Zen
    };
    let child_model = child_native.unwrap_or("muse-spark-1.2");
    store
        .set_subagent_model_setting(
            PersistenceMutationRequestId::from_bytes([0xb5; 16]),
            crate::persistence::SubagentModelSetting::Explicit {
                service: child_service,
                model_id: child_model.into(),
            },
        )
        .await
        .unwrap();
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0xb6; 16]),
            None,
            selected.path().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (started, observed) = oneshot::channel();
    let (resume, resumed) = oneshot::channel();
    let server = tokio::spawn(async move {
        let mut started = Some(started);
        let mut resumed = Some(resumed);
        for step in 0..4 {
            let (mut stream, _) = time::timeout(TERMINAL_RUN_TEST_TIMEOUT, listener.accept())
                .await
                .unwrap()
                .unwrap();
            let request = String::from_utf8(read_http_request(&mut stream).await).unwrap();
            let child = step == 1 || step == 2;
            let native = if child { child_native } else { parent_native };
            assert!(request.starts_with(if native.is_some() {
                "POST /backend-api/codex/responses"
            } else {
                "POST /zen/v1/responses"
            }));
            let body: serde_json::Value =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(body["model"], native.unwrap_or("muse-spark-1.2"));
            let output=match step {
                    0=> serde_json::json!({"id":"fc_task","type":"function_call","status":"completed","call_id":"mixed_task","name":"task","arguments":serde_json::json!({"context":"Independent check","tasks":[{"name":null,"task":"Read child.txt and report"}]}).to_string()}).to_string(),
                    1=>{
                        started.take().unwrap().send(()).unwrap();resumed.take().unwrap().await.unwrap();
                        serde_json::json!({"id":"fc_read","type":"function_call","status":"completed","call_id":"mixed_read","name":"read","arguments":serde_json::json!({"path":"child.txt"}).to_string()}).to_string()
                    },
                    _=>serde_json::json!({"id":format!("msg_{step}"),"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Check completed.","annotations":[]}]}).to_string(),
                };
            if let Some(model) = native {
                write_native_model(&mut stream, &format!("resp_{step}"), &output, model).await;
            } else {
                write_provider_output(&mut stream, &format!("resp_{step}"), &output).await;
            }
        }
    });
    let app = ServerApplication::from_native_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.id.as_bytes());
    let response = app
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0xb7; 16]),
            session_id,
            text: "Delegate one independent check".into(),
            attachments: Vec::new(),
            service: if native_parent {
                ModelService::OpenAiChatGpt
            } else {
                ModelService::Zen
            },
            model_id: parent_native.unwrap_or("muse-spark-1.2").into(),
        })
        .await
        .unwrap();
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        response
    else {
        panic!("expected accepted run")
    };
    time::timeout(TERMINAL_RUN_TEST_TIMEOUT, observed)
        .await
        .unwrap()
        .unwrap();
    app.execute_for_local_owner(ApplicationRequest::SetSubagentModelSetting {
        mutation_request_id: MutationRequestId::from_bytes([0xb8; 16]),
        setting: SubagentModelSetting::InheritParent {},
    })
    .await
    .unwrap();
    resume.send(()).unwrap();
    assert_eq!(
        wait_for_terminal(&app, session_id, run.id).await,
        RunState::Succeeded
    );
    server.await.unwrap();
    app.shutdown().await;
    drop(app);
    let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    let binding: (i64, i64, String) = db
        .query_row(
            "SELECT credential_kind,credential_generation,model_id FROM task_model_bindings",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        binding,
        (
            if child_native.is_some() { 2 } else { 1 },
            if child_native.is_some() { 3 } else { 1 },
            child_model.into()
        )
    );
    let call: [u8; 16] = db
        .query_row("SELECT call_id FROM task_model_bindings", [], |row| {
            row.get(0)
        })
        .unwrap();
    let store =
        SessionStore::open_for_test(root.path()).expect("mixed-provider history should reopen");
    assert!(
        store
            .task_model_binding(
                crate::persistence::RunId::from_bytes([0xee; 16]),
                crate::persistence::ToolCallId::from_bytes(call)
            )
            .await
            .is_err()
    );
    if native_parent {
        db.execute(
            "UPDATE task_model_bindings SET binding_digest = zeroblob(32)",
            [],
        )
        .unwrap();
    } else {
        db.execute("DELETE FROM task_model_bindings", []).unwrap();
    }
    assert!(
        store
            .task_model_binding(
                crate::persistence::RunId::from_bytes(*run.id.as_bytes()),
                crate::persistence::ToolCallId::from_bytes(call)
            )
            .await
            .is_err()
    );
    drop(db);
    drop(store);
    assert!(
        SessionStore::open_for_test(root.path()).is_err(),
        "missing/corrupt new bindings must fail closed"
    );
}
