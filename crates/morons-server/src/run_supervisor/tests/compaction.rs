use super::*;

#[tokio::test(flavor = "current_thread")]
async fn manual_compaction_runs_below_threshold_with_bounded_user_guidance() {
    let root = TestRoot::new("manual-context-compaction");
    let selected = TestRoot::new("manual-context-compaction-directory");
    let store = SessionStore::open_for_test(root.path()).expect("session store should open");
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0xd1; 16]),
            0,
            b"not-a-real-manual-compaction-key".to_vec(),
        )
        .await
        .expect("credential should be configured");
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0xd2; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    append_completed_context_run(&store, session.id, 0xd3, "OLD_MANUAL_ONE", 100).await;
    append_completed_context_run(&store, session.id, 0xd4, "RECENT_MANUAL_TWO", 100).await;
    append_completed_context_run(&store, session.id, 0xd5, "RECENT_MANUAL_THREE", 100).await;
    let (base, requests, provider_task) = spawn_compaction_provider().await;
    let application = ServerApplication::from_session_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.id.as_bytes());
    let before = application
        .execute_for_local_owner(ApplicationRequest::GetSessionContext {
            session_id,
            service: OpenCodeService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .expect("context status should load");
    let ApplicationOutcome::Response(ApplicationResponse::SessionContextFound { context: before }) =
        before
    else {
        panic!("context status should be returned");
    };
    assert!(before.estimated_input_tokens < before.compaction_threshold_tokens);
    assert_eq!(before.checkpoint_source_entry_high_water, None);

    let accepted = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0xd6; 16]),
            session_id,
            text: "/compact preserve the frog migration decision".to_owned(),
            attachments: Vec::new(),
            service: OpenCodeService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .expect("manual compaction should be accepted");
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        accepted
    else {
        panic!("manual compaction should return a run");
    };
    assert_eq!(
        wait_for_terminal(&application, session_id, run.id).await,
        RunState::Succeeded
    );
    provider_task.await.expect("provider should finish");
    let requests = requests.await.expect("requests should be captured");
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("Untrusted user-requested summary emphasis"));
    assert!(requests[0].contains("preserve the frog migration decision"));
    assert!(requests[0].contains("OLD_MANUAL_ONE"));
    assert!(!requests[0].contains("RECENT_MANUAL_TWO"));

    let after = application
        .execute_for_local_owner(ApplicationRequest::GetSessionContext {
            session_id,
            service: OpenCodeService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .expect("compacted context status should load");
    let ApplicationOutcome::Response(ApplicationResponse::SessionContextFound { context: after }) =
        after
    else {
        panic!("context status should be returned");
    };
    assert!(after.checkpoint_source_entry_high_water.is_some());
    assert!(after.checkpoint_estimated_summary_tokens.is_some());
    application.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn proactive_compaction_commits_a_source_bound_summary_and_uses_recent_tail() {
    let root = TestRoot::new("context-compaction");
    let selected = TestRoot::new("context-compaction-directory");
    let store = SessionStore::open_for_test(root.path()).expect("session store should open");
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0xe1; 16]),
            0,
            b"not-a-real-compaction-key".to_vec(),
        )
        .await
        .expect("credential should be configured");
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0xe2; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    append_completed_context_run(&store, session.id, 0xe3, "OLD_RUN_ONE", 24_000).await;
    let hidden = store
        .accept_local_command(
            PersistenceMutationRequestId::from_bytes([0xe7; 16]),
            session.id,
            "SECRET_CONTEXT_EXCLUDED".to_owned(),
            false,
        )
        .await
        .expect("context-excluded command should be accepted");
    assert!(
        store
            .activate_local_command(hidden.id)
            .await
            .expect("context-excluded command should activate")
    );
    store
        .complete_local_command(
            hidden.id,
            crate::tools::ToolResult::Ok {
                output: crate::tools::ToolOutput::Bash {
                    exit_code: Some(0),
                    signal: None,
                    stdout: "SECRET_CONTEXT_EXCLUDED_OUTPUT".to_owned(),
                    stderr: String::new(),
                },
            },
        )
        .await
        .expect("context-excluded command should complete");
    append_completed_context_run(&store, session.id, 0xe4, "RECENT_RUN_TWO", 24_000).await;
    append_completed_context_run(&store, session.id, 0xe5, "RECENT_RUN_THREE", 24_000).await;
    let (base, requests, provider_task) = spawn_compaction_provider().await;
    let application = ServerApplication::from_session_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.id.as_bytes());
    let accepted = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0xe6; 16]),
            session_id,
            text: "CURRENT_RUN_FOUR".to_owned(),
            attachments: Vec::new(),
            service: OpenCodeService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .expect("compacting run should be accepted");
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
        .expect("compaction provider should finish");
    let requests = requests.await.expect("requests should be captured");
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("Summarize the supplied earlier session prefix"));
    assert!(requests[0].contains("OLD_RUN_ONE"));
    assert!(!requests[0].contains("SECRET_CONTEXT_EXCLUDED"));
    assert!(!requests[0].contains("CURRENT_RUN_FOUR"));
    assert!(requests[1].contains("SUMMARY_OF_OLDEST_PREFIX"));
    assert!(!requests[1].contains("OLD_RUN_ONE"));
    assert!(requests[1].contains("RECENT_RUN_TWO"));
    assert!(requests[1].contains("RECENT_RUN_THREE"));
    assert!(requests[1].contains("CURRENT_RUN_FOUR"));
    application.shutdown().await;
    drop(application);
    let reopened =
        SessionStore::open_for_test(root.path()).expect("compaction checkpoint should reopen");
    drop(reopened);
    let connection = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3"))
        .expect("database should open for corruption");
    connection
        .execute(
            "UPDATE context_checkpoints SET source_digest = ?1",
            [&[0_u8; 32][..]],
        )
        .expect("checkpoint digest should be corrupted");
    drop(connection);
    assert!(SessionStore::open_for_test(root.path()).is_err());
}
