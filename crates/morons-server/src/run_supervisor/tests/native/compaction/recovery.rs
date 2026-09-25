use super::*;

#[tokio::test(flavor = "current_thread")]
async fn proven_overflow_recovers_once_and_reopens() {
    for (repeat, compaction_fails, cancel) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        recover(repeat, compaction_fails, cancel).await;
    }
}

async fn recover(repeat: bool, compaction_fails: bool, cancel: bool) {
    let model = "gpt-5.5";
    let (root, _selected, store, session, _) = fixture(false, model).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (settled, mut settlement) = tokio::sync::oneshot::channel();
    let (compacting, compacting_rx) = tokio::sync::oneshot::channel();
    let mut compacting = Some(compacting);
    let server = tokio::spawn(async move {
        for step in 0..if compaction_fails || cancel { 2 } else { 3 } {
            let (mut stream, _) = time::timeout(TERMINAL_RUN_TEST_TIMEOUT, listener.accept())
                .await
                .unwrap()
                .unwrap();
            let request = String::from_utf8(read_http_request(&mut stream).await).unwrap();
            let body: serde_json::Value =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(body["model"], model);
            assert_eq!(body["tools"].as_array().unwrap().is_empty(), step == 1);
            if step == 2 {
                assert!(body["input"].to_string().contains("RECOVERED_SUMMARY"));
                assert!(body["input"].to_string().contains("continue"));
            }
            if step == 1 && cancel {
                compacting.take().unwrap().send(()).unwrap();
                tokio::select! {
                    biased;
                    _ = listener.accept() => panic!("cancelled compaction must not redispatch"),
                    result = &mut settlement => result.unwrap(),
                }
                return;
            }
            if step == 0 || (step == 2 && repeat) || (step == 1 && compaction_fails) {
                let rejection = r#"{"error":{"code":"context_length_exceeded"}}"#;
                stream.write_all(format!("HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{rejection}", rejection.len()).as_bytes()).await.unwrap();
                stream.shutdown().await.unwrap();
            } else {
                write_native_model(
                    &mut stream,
                    &format!("resp_recovery_{step}"),
                    &output(if step == 1 {
                        "RECOVERED_SUMMARY"
                    } else {
                        "Done."
                    }),
                    model,
                )
                .await;
            }
        }
        tokio::select! {
            biased;
            _ = listener.accept() => panic!("recovery must be bounded"),
            result = &mut settlement => result.unwrap(),
        }
    });
    let app = ServerApplication::from_native_shared_for_test(store.clone(), &base);
    let session_id = SessionId::from_bytes(*session.as_bytes());
    let response = app
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0xa3; 16]),
            session_id,
            text: "continue".into(),
            attachments: Vec::new(),
            service: ModelService::OpenAiChatGpt,
            model_id: model.into(),
        })
        .await
        .unwrap();
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        response
    else {
        panic!("expected run")
    };
    if cancel {
        time::timeout(TERMINAL_RUN_TEST_TIMEOUT, compacting_rx)
            .await
            .unwrap()
            .unwrap();
        app.execute_for_local_owner(ApplicationRequest::CancelRun {
            mutation_request_id: MutationRequestId::from_bytes([0xa4; 16]),
            session_id,
            run_id: run.id,
        })
        .await
        .unwrap();
    }
    assert_eq!(
        wait_for_terminal(&app, session_id, run.id).await,
        if cancel {
            RunState::Cancelled
        } else if repeat || compaction_fails {
            RunState::Failed
        } else {
            RunState::Succeeded
        }
    );
    app.shutdown().await;
    settled.send(()).unwrap();
    server.await.unwrap();
    drop(app);
    drop(store);
    let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    let facts = db.prepare("SELECT fact_kind FROM provider_operation_facts WHERE run_id = ?1 ORDER BY fact_sequence").unwrap().query_map([run.id.as_bytes().as_slice()], |row| row.get::<_, i64>(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(
        facts,
        if compaction_fails || cancel {
            vec![1, 2, 4]
        } else {
            vec![1, 2, 4, 1, 2, if repeat { 4 } else { 3 }]
        }
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM compaction_operations WHERE state = 3",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        i64::from(!compaction_fails && !cancel)
    );
    drop(db);
    drop(SessionStore::open_for_test(root.path()).expect("recovery must reopen"));
}

#[tokio::test(flavor = "current_thread")]
async fn restart_after_rejection_settlement_never_resumes_recovery() {
    let (root, _selected, store, session, _) = fixture(false, "gpt-5.5").await;
    let run = store
        .accept_session_input(
            PersistenceMutationRequestId::from_bytes([0xa6; 16]),
            session,
            "continue".into(),
            selection("gpt-5.5"),
        )
        .await
        .unwrap()
        .run;
    store.activate_run(run.id).await.unwrap();
    let context = store.load_run_context(run.id).await.unwrap();
    let PrepareOperationOutcome::Prepared(operation) = store
        .prepare_provider_operation(
            run.id,
            context.current_entry_high_water,
            context.estimated_input_tokens,
        )
        .await
        .unwrap()
    else {
        panic!("expected preparation")
    };
    store
        .mark_provider_dispatched(run.id, operation)
        .await
        .unwrap();
    assert!(
        store
            .settle_context_rejection(run.id, operation)
            .await
            .unwrap()
    );
    assert!(
        !store
            .settle_context_rejection(run.id, operation)
            .await
            .unwrap()
    );
    assert!(
        store
            .load_run_context(run.id)
            .await
            .unwrap()
            .compaction_plan
            .is_some()
    );
    drop(store);
    let reopened = SessionStore::open_for_test(root.path()).unwrap();
    let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    let (terminal, attempts): (bool, i64) = db.query_row(
        "SELECT state NOT IN (1, 2), (SELECT COUNT(*) FROM provider_operation_facts WHERE run_id = ?1 AND fact_kind = 1) FROM runs WHERE run_id = ?1",
        [run.id.as_bytes().as_slice()], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert!(terminal);
    assert_eq!(attempts, 1);
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM compaction_operations", [], |row| row
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        0
    );
    drop(db);
    drop(reopened);
}
