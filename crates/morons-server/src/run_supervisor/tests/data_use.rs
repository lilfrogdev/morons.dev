use super::*;

#[tokio::test(flavor = "current_thread")]
async fn policy_change_blocks_the_next_root_turn_without_uncertain_failure_or_replay() {
    exercise(false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn policy_change_blocks_a_child_but_does_not_substitute_its_model_or_block_a_compliant_parent()
 {
    exercise(true).await;
}

async fn exercise(child: bool) {
    let root = TestRoot::new("policy-application");
    let selected = TestRoot::new("policy-application-selected");
    fs::write(selected.path().join("sample.txt"), "preserved source").unwrap();
    let store = SessionStore::open_for_test(root.path()).unwrap();
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0xd1; 16]),
            0,
            b"synthetic-policy-app-key".to_vec(),
        )
        .await
        .unwrap();
    store
        .set_subagent_model_setting(
            PersistenceMutationRequestId::from_bytes([0xd2; 16]),
            crate::persistence::SubagentModelSetting::Explicit {
                service: RunService::Go,
                model_id: "gpt-5.6-luna".into(),
            },
        )
        .await
        .unwrap();
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0xd3; 16]),
            None,
            selected.path().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (seen, seen_response) = oneshot::channel();
    let (release, release_response) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = String::from_utf8(read_http_request(&mut stream).await).unwrap();
        assert!(request.starts_with(if child {
            "POST /zen/v1/responses"
        } else {
            "POST /zen/go/v1/responses"
        }));
        seen.send(()).unwrap();
        release_response.await.unwrap();
        let (name, arguments) = if child {
            (
                "task",
                serde_json::json!({"context":"A bounded check","tasks":[{"name":null,"task":"Report briefly"}]}),
            )
        } else {
            ("read", serde_json::json!({"path":"sample.txt"}))
        };
        let output=serde_json::json!({"id":"fc_policy","type":"function_call","status":"completed","call_id":"call_policy","name":name,"arguments":arguments.to_string()}).to_string();
        let body = provider_output_body("resp_policy", &output);
        let body = if child {
            body
        } else {
            body.replace("muse-spark-1.2", "gpt-5.6-luna")
        };
        write_provider_headers(&mut stream, body.len()).await;
        stream.write_all(body.as_bytes()).await.unwrap();
        stream.shutdown().await.unwrap();
        if child {
            let (mut stream, _) = time::timeout(TERMINAL_RUN_TEST_TIMEOUT, listener.accept())
                .await
                .unwrap()
                .unwrap();
            let request = String::from_utf8(read_http_request(&mut stream).await).unwrap();
            assert!(
                request.starts_with("POST /zen/v1/responses"),
                "no child request or model substitution may dispatch"
            );
            assert!(request.contains("subagent model does not satisfy data-use restrictions"));
            assert!(request.contains("gpt-5.6-luna"));
            let output = r#"{"id":"msg_policy","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"The child was blocked by policy.","annotations":[]}]}"#;
            write_provider_output(&mut stream, "resp_policy_final", output).await;
        }
    });
    let application = ServerApplication::from_session_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.id.as_bytes());
    let outcome = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0xd4; 16]),
            session_id,
            text: "Perform a bounded check".into(),
            attachments: Vec::new(),
            service: if child {
                ModelService::Zen
            } else {
                ModelService::Go
            },
            model_id: if child {
                "muse-spark-1.2"
            } else {
                "gpt-5.6-luna"
            }
            .into(),
        })
        .await
        .unwrap();
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        outcome
    else {
        panic!("expected accepted input")
    };
    time::timeout(TERMINAL_RUN_TEST_TIMEOUT, seen_response)
        .await
        .unwrap()
        .unwrap();
    application
        .execute_for_local_owner(ApplicationRequest::SetDataUsePolicy {
            mutation_request_id: MutationRequestId::from_bytes([0xd5; 16]),
            policy: morons_protocol::DataUsePolicy {
                sequence: 0,
                block_training_use: false,
                require_zero_retention: true,
            },
        })
        .await
        .unwrap();
    release.send(()).unwrap();
    assert_eq!(
        wait_for_terminal(&application, session_id, run.id).await,
        if child {
            RunState::Succeeded
        } else {
            RunState::Failed
        }
    );
    time::timeout(TERMINAL_RUN_TEST_TIMEOUT, server)
        .await
        .unwrap()
        .unwrap();
    application.shutdown().await;
    let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM provider_operation_facts WHERE fact_kind=2",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        if child { 2 } else { 1 }
    );
    if !child {
        assert_eq!(
            db.query_row("SELECT failure_kind FROM runs", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            12
        );
    }
    assert_eq!(
        fs::read_to_string(selected.path().join("sample.txt")).unwrap(),
        "preserved source"
    );
}
