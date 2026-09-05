use super::*;

#[tokio::test(flavor = "current_thread")]
async fn task_tool_runs_scoped_children_and_commits_only_bounded_reports() {
    let root = TestRoot::new("subagent-tool-loop");
    let selected = TestRoot::new("subagent-tool-directory");
    fs::write(selected.path().join("alpha.txt"), "alpha source\n")
        .expect("subagent fixture file should be written");
    let store = SessionStore::open_for_test(root.path()).expect("session store should open");
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0x74; 16]),
            0,
            b"not-a-real-subagent-key".to_vec(),
        )
        .await
        .expect("credential should be configured");
    store
        .set_subagent_model_setting(
            PersistenceMutationRequestId::from_bytes([0x73; 16]),
            crate::persistence::SubagentModelSetting::OpenCode {
                service: RunOpenCodeService::Go,
                model_id: "glm-5.3-flash".to_owned(),
            },
        )
        .await
        .expect("cross-protocol subagent model should be configured");
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0x75; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let (base, requests, provider_task) = spawn_subagent_provider().await;
    let application = ServerApplication::from_session_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.id.as_bytes());
    let accepted = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0x76; 16]),
            session_id,
            text: "Delegate two independent checks.".to_owned(),
            attachments: Vec::new(),
            service: OpenCodeService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .expect("subagent run should be accepted");
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        accepted
    else {
        panic!("input should return a run");
    };
    assert_eq!(
        wait_for_terminal(&application, session_id, run.id).await,
        RunState::Succeeded
    );
    provider_task
        .await
        .expect("subagent provider fixture should finish");
    let requests = requests.await.expect("requests should be captured");
    assert_eq!(requests.len(), 5);
    assert!(requests[0].contains("\"name\":\"task\""));
    assert!(
        requests[1..3]
            .iter()
            .all(|request| request.contains("Shared context:"))
    );
    assert!(
        requests[1..3]
            .iter()
            .all(|request| !request.contains("Delegate two independent checks."))
    );
    assert!(
        requests[1..3]
            .iter()
            .all(|request| !request.contains("\"name\":\"task\""))
    );
    assert!(
        requests[1..3]
            .iter()
            .all(|request| !request.contains("\"name\":\"ipython\""))
    );
    assert!(requests[3].contains("\"role\":\"tool\""));
    assert!(requests[3].contains("alpha source"));
    assert!(requests[4].contains("alpha report"));
    assert!(requests[4].contains("beta report"));
    assert!(requests[4].contains("OpenCode Go"));
    assert!(requests[4].contains("glm-5.3-flash"));
    assert!(requests[0].starts_with("POST /zen/v1/responses"));
    assert!(
        requests[1..4]
            .iter()
            .all(|request| request.starts_with("POST /zen/go/v1/chat/completions"))
    );
    assert!(
        requests[4]
            .find("alpha report")
            .zip(requests[4].find("beta report"))
            .is_some_and(|(alpha, beta)| alpha < beta)
    );
    let headers = requests
        .iter()
        .map(|request| request_header(request, "x-opencode-session"))
        .collect::<Vec<_>>();
    let alpha_index = if requests[1].contains("alpha report") {
        1
    } else {
        2
    };
    let beta_index = if alpha_index == 1 { 2 } else { 1 };
    assert_eq!(headers[0], headers[4]);
    assert_eq!(headers[alpha_index], headers[3]);
    assert_ne!(headers[0], headers[alpha_index]);
    assert_ne!(headers[0], headers[beta_index]);
    assert_ne!(headers[alpha_index], headers[beta_index]);

    let mut cursor = None;
    let mut entries = Vec::new();
    loop {
        let outcome = application
            .execute_for_local_owner(ApplicationRequest::ListSessionTranscript {
                session_id,
                cursor,
                direction: morons_protocol::TranscriptPageDirection::Newer,
                limit: 1,
            })
            .await
            .expect("subagent transcript should page");
        let ApplicationOutcome::Response(ApplicationResponse::SessionTranscriptListed {
            entries: page,
            newer_cursor,
            ..
        }) = outcome
        else {
            panic!("transcript should return a page");
        };
        entries.extend(page);
        let Some(next) = newer_cursor else { break };
        cursor = Some(next);
    }
    assert_eq!(entries.len(), 4);
    assert!(matches!(
        &entries[1],
        morons_protocol::TranscriptEntry::ToolCall {
            tool: morons_protocol::ToolKind::Task,
            path,
            ..
        } if path == "2 subagent tasks"
    ));
    assert!(matches!(
        &entries[2],
        morons_protocol::TranscriptEntry::ToolResult {
            tool: morons_protocol::ToolKind::Task,
            status: morons_protocol::ToolResultStatus::Succeeded,
            summary,
            ..
        } if summary.find("alpha report").zip(summary.find("beta report"))
            .is_some_and(|(alpha, beta)| alpha < beta)
            && summary.contains("OpenCode Go / glm-5.3-flash · protocol revision 2")
    ));
    application.shutdown().await;
    drop(application);
    SessionStore::open_for_test(root.path()).expect("durable subagent result should reopen");
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_a_parent_run_stops_its_subagent_batch() {
    let root = TestRoot::new("subagent-cancellation");
    let selected = TestRoot::new("subagent-cancellation-directory");
    let store = SessionStore::open_for_test(root.path()).expect("session store should open");
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0x77; 16]),
            0,
            b"not-a-real-subagent-cancellation-key".to_vec(),
        )
        .await
        .expect("credential should be configured");
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0x78; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let (base, child_dispatched, provider_task) = spawn_stalled_subagent_provider().await;
    let application = ServerApplication::from_session_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.id.as_bytes());
    let accepted = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0x79; 16]),
            session_id,
            text: "Delegate a stalled check.".to_owned(),
            attachments: Vec::new(),
            service: OpenCodeService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .expect("subagent run should be accepted");
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        accepted
    else {
        panic!("input should return a run");
    };
    time::timeout(TERMINAL_RUN_TEST_TIMEOUT, child_dispatched)
        .await
        .expect("child should dispatch")
        .expect("child dispatch should be observed");
    application
        .execute_for_local_owner(ApplicationRequest::CancelRun {
            mutation_request_id: MutationRequestId::from_bytes([0x7a; 16]),
            session_id,
            run_id: run.id,
        })
        .await
        .expect("parent cancellation should be accepted");
    assert_eq!(
        wait_for_terminal(&application, session_id, run.id).await,
        RunState::Cancelled
    );
    provider_task
        .await
        .expect("stalled child provider fixture should finish");
    application.shutdown().await;
}
