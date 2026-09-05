use super::*;

#[tokio::test(flavor = "current_thread")]
async fn complete_provider_outcome_commits_assistant_and_terminal_run() {
    let root = TestRoot::new("run-completion");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    configure_credential(&store).await;
    let session = store
        .create_session(MutationRequestId::from_bytes([0x21; 16]), None)
        .await
        .expect("session should be created");
    let accepted = store
        .accept_session_input(
            MutationRequestId::from_bytes([0x22; 16]),
            session.id,
            "answer this".to_owned(),
            model_selection(),
        )
        .await
        .expect("input should be accepted");
    assert_eq!(
        store
            .activate_run(accepted.run.id)
            .await
            .expect("run should activate"),
        ActivationOutcome::Active
    );
    let context = store
        .load_run_context(accepted.run.id)
        .await
        .expect("run context should load");
    assert_eq!(context.entries.len(), 1);
    let operation_id = match store
        .prepare_provider_operation(
            accepted.run.id,
            context.current_entry_high_water,
            context.estimated_input_tokens,
        )
        .await
        .expect("provider operation should prepare")
    {
        PrepareOperationOutcome::Prepared(operation_id) => operation_id,
        other => panic!("unexpected preparation outcome: {other:?}"),
    };
    assert_eq!(
        store
            .mark_provider_dispatched(accepted.run.id, operation_id)
            .await
            .expect("provider operation should dispatch"),
        DispatchOutcome::Dispatched
    );
    let completed = store
        .complete_run_success(
            accepted.run.id,
            operation_id,
            CompletedAssistant {
                text: "durable answer".to_owned(),
                refusal: false,
                provider_response_id: "resp_test".to_owned(),
                usage: ProviderUsage {
                    input_tokens: 10,
                    cached_input_tokens: 0,
                    cache_write_input_tokens: 0,
                    output_tokens: 4,
                    reasoning_output_tokens: 0,
                    total_tokens: 14,
                },
            },
        )
        .await
        .expect("provider outcome should complete");
    assert_eq!(completed.state, RunState::Succeeded);
    let retry = store
        .find_session_input_retry(
            MutationRequestId::from_bytes([0x22; 16]),
            session.id,
            "answer this",
            RunOpenCodeService::Zen,
            TEST_MODEL,
        )
        .await
        .expect("terminal input retry should resolve")
        .expect("terminal input retry should exist");
    assert_eq!(retry.run.id, accepted.run.id);
    assert_eq!(retry.run.state, RunState::Accepted);

    let first = store
        .list_session_transcript(session.id, None, 1)
        .await
        .expect("first transcript page should load");
    assert_eq!(first.session.id, session.id);
    assert_eq!(first.active_run_id, None);
    assert!(matches!(
        &first.runs[..],
        [run] if run.id == accepted.run.id && run.state == RunState::Succeeded
    ));
    let events = store
        .read_session_events(session.id, SessionEventCursor::new(session.id, 0), 100)
        .await
        .expect("session events should replay");
    assert_eq!(events.events.len(), 5);
    assert!(matches!(
        &events.events[0].payload,
        SessionEventPayload::TranscriptEntry(TranscriptEntry::UserMessage { run_id, .. })
            if *run_id == accepted.run.id
    ));
    assert!(matches!(
        &events.events[1].payload,
        SessionEventPayload::RunChanged(run) if run.state == RunState::Accepted
    ));
    assert!(matches!(
        &events.events[2].payload,
        SessionEventPayload::RunChanged(run) if run.state == RunState::Active
    ));
    assert!(matches!(
        &events.events[3].payload,
        SessionEventPayload::TranscriptEntry(TranscriptEntry::AssistantMessage { run_id, .. })
            if *run_id == accepted.run.id
    ));
    assert!(matches!(
        &events.events[4].payload,
        SessionEventPayload::RunChanged(run) if run.state == RunState::Succeeded
    ));
    assert_eq!(events.high_water, first.event_cursor);
    let continuation = first
        .next_cursor
        .expect("completed transcript should have another page");
    let forged_cursor = TranscriptCursor::new(
        session.id,
        continuation.snapshot_entry_sequence(),
        0,
        continuation.after_entry_sequence(),
    );
    let forged_snapshot = store
        .list_session_transcript(session.id, Some(forged_cursor), 1)
        .await
        .expect_err("inconsistent transcript high waters should fail");
    assert!(matches!(
        forged_snapshot,
        PersistenceError::InvalidInput { .. }
    ));
    let other_session = store
        .create_session(MutationRequestId::from_bytes([0x24; 16]), None)
        .await
        .expect("second session should be created");
    let cross_session = store
        .list_session_transcript(other_session.id, first.next_cursor, 1)
        .await
        .expect_err("cross-session transcript cursor should fail");
    assert!(matches!(
        cross_session,
        PersistenceError::InvalidInput { .. }
    ));
    let cross_session_events = store
        .read_session_events(other_session.id, first.event_cursor, 1)
        .await
        .expect_err("cross-session event cursor should fail");
    assert!(matches!(
        cross_session_events,
        PersistenceError::InvalidInput { .. }
    ));
    let second = store
        .list_session_transcript(session.id, first.next_cursor, 1)
        .await
        .expect("second transcript page should load");
    assert!(first.next_cursor.is_some());
    assert!(second.next_cursor.is_none());
    assert_eq!(second.event_cursor, first.event_cursor);
    assert_eq!(second.active_run_id, None);
    assert!(matches!(
        &second.entries[0],
        TranscriptEntry::AssistantMessage { run_id, text, .. }
            if *run_id == accepted.run.id && text == "durable answer"
    ));

    let latest = store
        .list_session_transcript_window(session.id, None, TranscriptPageDirection::Older, 1)
        .await
        .expect("latest transcript window should load");
    assert!(latest.newer_cursor.is_none());
    let older_cursor = latest
        .older_cursor
        .expect("latest entry should have older history");
    assert!(matches!(
        &latest.entries[..],
        [TranscriptEntry::AssistantMessage { text, .. }] if text == "durable answer"
    ));
    let older = store
        .list_session_transcript_window(
            session.id,
            Some(older_cursor),
            TranscriptPageDirection::Older,
            1,
        )
        .await
        .expect("older transcript window should load");
    assert!(older.older_cursor.is_none());
    let newer_cursor = older
        .newer_cursor
        .expect("oldest entry should link back to newer history");
    assert!(matches!(
        &older.entries[..],
        [TranscriptEntry::UserMessage { text, .. }] if text == "answer this"
    ));
    let newer = store
        .list_session_transcript_window(
            session.id,
            Some(newer_cursor),
            TranscriptPageDirection::Newer,
            1,
        )
        .await
        .expect("newer transcript window should load");
    assert!(newer.newer_cursor.is_none());
    assert!(newer.older_cursor.is_some());
    assert!(matches!(
        &newer.entries[..],
        [TranscriptEntry::AssistantMessage { text, .. }] if text == "durable answer"
    ));

    let next = store
        .accept_session_input(
            MutationRequestId::from_bytes([0x23; 16]),
            session.id,
            "continue deterministically".to_owned(),
            model_selection(),
        )
        .await
        .expect("a new run should be accepted after success");
    store
        .activate_run(next.run.id)
        .await
        .expect("next run should activate");
    let fixed_snapshot = store
        .list_session_transcript(session.id, first.next_cursor, 1)
        .await
        .expect("fixed transcript snapshot should remain readable");
    assert!(fixed_snapshot.next_cursor.is_none());
    assert_eq!(fixed_snapshot.event_cursor, first.event_cursor);
    assert_eq!(fixed_snapshot.active_run_id, None);
    assert!(matches!(
        &fixed_snapshot.runs[..],
        [run] if run.id == accepted.run.id && run.state == RunState::Succeeded
    ));
    assert!(matches!(
        &fixed_snapshot.entries[..],
        [TranscriptEntry::AssistantMessage { run_id, .. }] if *run_id == accepted.run.id
    ));
    let context = store
        .load_run_context(next.run.id)
        .await
        .expect("next run context should load");
    assert_eq!(context.entries.len(), 3);
    assert!(matches!(
        &context.entries[..],
        [
            TranscriptEntry::UserMessage { .. },
            TranscriptEntry::AssistantMessage { .. },
            TranscriptEntry::UserMessage { run_id, .. },
        ] if *run_id == next.run.id
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_is_exact_durable_and_stops_before_dispatch() {
    let root = TestRoot::new("run-cancellation");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    configure_credential(&store).await;
    let session = store
        .create_session(MutationRequestId::from_bytes([0x31; 16]), None)
        .await
        .expect("session should be created");
    let accepted = store
        .accept_session_input(
            MutationRequestId::from_bytes([0x32; 16]),
            session.id,
            "cancel this".to_owned(),
            model_selection(),
        )
        .await
        .expect("input should be accepted");
    store
        .activate_run(accepted.run.id)
        .await
        .expect("run should activate");
    let context = store
        .load_run_context(accepted.run.id)
        .await
        .expect("run context should load");
    let operation_id = match store
        .prepare_provider_operation(
            accepted.run.id,
            context.current_entry_high_water,
            context.estimated_input_tokens,
        )
        .await
        .expect("provider operation should prepare")
    {
        PrepareOperationOutcome::Prepared(operation_id) => operation_id,
        other => panic!("unexpected preparation outcome: {other:?}"),
    };
    let cancellation_id = MutationRequestId::from_bytes([0x33; 16]);
    let cancellation = store
        .cancel_run(cancellation_id, session.id, accepted.run.id)
        .await
        .expect("cancellation intent should commit");
    assert!(cancellation.intent_applied);
    assert_eq!(cancellation.state, RunState::Active);
    assert!(cancellation.cancellation_requested);
    let retry = store
        .cancel_run(cancellation_id, session.id, accepted.run.id)
        .await
        .expect("cancellation retry should resolve");
    assert_eq!(retry, cancellation);
    assert_eq!(
        store
            .mark_provider_dispatched(accepted.run.id, operation_id)
            .await
            .expect("dispatch boundary should observe cancellation"),
        DispatchOutcome::Cancelled
    );
    let run = store
        .get_run(session.id, accepted.run.id)
        .await
        .expect("run query should succeed")
        .expect("run should exist");
    assert_eq!(run.state, RunState::Cancelled);
    let events = store
        .read_session_events(session.id, SessionEventCursor::new(session.id, 0), 100)
        .await
        .expect("cancellation events should replay");
    assert!(matches!(
        &events.events[3].payload,
        SessionEventPayload::RunChanged(run)
            if run.id == accepted.run.id
                && run.state == RunState::Active
                && run.cancellation_requested
    ));
    assert!(matches!(
        &events.events[4].payload,
        SessionEventPayload::RunChanged(run)
            if run.id == accepted.run.id && run.state == RunState::Cancelled
    ));
    let terminal = store
        .cancel_run(
            MutationRequestId::from_bytes([0x34; 16]),
            session.id,
            accepted.run.id,
        )
        .await
        .expect("terminal cancellation should return terminal state");
    assert_eq!(terminal.state, RunState::Cancelled);
    assert!(terminal.cancellation_requested);
    assert!(!terminal.intent_applied);
}

#[tokio::test(flavor = "current_thread")]
async fn startup_never_replays_a_dispatched_subagent_batch() {
    let root = TestRoot::new("subagent-run-recovery");
    let session_id;
    let run_id;
    {
        let store = SessionStore::open_at(root.path()).expect("session store should open");
        configure_credential(&store).await;
        let session = store
            .create_session(MutationRequestId::from_bytes([0x3a; 16]), None)
            .await
            .expect("session should be created");
        let accepted = store
            .accept_session_input(
                MutationRequestId::from_bytes([0x3b; 16]),
                session.id,
                "delegate work".to_owned(),
                model_selection(),
            )
            .await
            .expect("input should be accepted");
        store
            .activate_run(accepted.run.id)
            .await
            .expect("run should activate");
        let context = store
            .load_run_context(accepted.run.id)
            .await
            .expect("run context should load");
        let provider_operation_id = match store
            .prepare_provider_operation(
                accepted.run.id,
                context.current_entry_high_water,
                context.estimated_input_tokens,
            )
            .await
            .expect("provider operation should prepare")
        {
            PrepareOperationOutcome::Prepared(operation_id) => operation_id,
            other => panic!("unexpected preparation outcome: {other:?}"),
        };
        assert_eq!(
            store
                .mark_provider_dispatched(accepted.run.id, provider_operation_id)
                .await
                .expect("provider operation should dispatch"),
            DispatchOutcome::Dispatched
        );
        let committed = store
            .complete_provider_tool_turn(
                accepted.run.id,
                provider_operation_id,
                super::CompletedToolTurn {
                    provider_response_id: "resp_subagent_recovery".to_owned(),
                    usage: ProviderUsage {
                        input_tokens: 10,
                        cached_input_tokens: 0,
                        cache_write_input_tokens: 0,
                        output_tokens: 2,
                        reasoning_output_tokens: 0,
                        total_tokens: 12,
                    },
                    commentary: None,
                    calls: vec![crate::tools::ValidatedProviderCall {
                        provider_call_id: "provider_task_recovery".to_owned(),
                        input: crate::tools::ToolInput::Task {
                            context: "shared context".to_owned(),
                            tasks: vec![crate::tools::SubagentTask {
                                name: Some("worker".to_owned()),
                                task: "inspect state".to_owned(),
                            }],
                        },
                        opaque_continuation: Some("ephemeral-provider-continuation".to_owned()),
                    }],
                },
            )
            .await
            .expect("task call should commit");
        let call = &committed.calls[0];
        assert_eq!(
            call.opaque_continuation.as_deref(),
            Some("ephemeral-provider-continuation")
        );
        assert!(!format!("{call:?}").contains("ephemeral-provider-continuation"));
        store
            .prepare_tool_operation(accepted.run.id, call.call_id, call.operation_id, None)
            .await
            .expect("task operation should prepare");
        store
            .mark_tool_dispatched(accepted.run.id, call.call_id, call.operation_id)
            .await
            .expect("task operation should dispatch");
        session_id = session.id;
        run_id = accepted.run.id;
    }

    let store = SessionStore::open_at(root.path()).expect("recovery should complete");
    let run = store
        .get_run(session_id, run_id)
        .await
        .expect("run query should succeed")
        .expect("run should remain");
    assert_eq!(run.state, RunState::Uncertain);
    let mut cursor = None;
    let mut entries = Vec::new();
    loop {
        let page = store
            .list_session_transcript(session_id, cursor, 1)
            .await
            .expect("transcript should page");
        entries.extend(page.entries);
        let Some(next) = page.next_cursor else { break };
        cursor = Some(next);
    }
    assert!(matches!(
        entries.last(),
        Some(TranscriptEntry::ToolResult {
            tool: crate::tools::ToolKind::Task,
            result: crate::tools::ToolResult::Error {
                error: crate::tools::ToolErrorKind::Uncertain,
                ..
            },
            ..
        })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn startup_interrupts_nonterminal_runs_without_redispatch() {
    let root = TestRoot::new("run-recovery");
    let run_id;
    let session_id;
    {
        let store = SessionStore::open_at(root.path()).expect("session store should open");
        configure_credential(&store).await;
        let session = store
            .create_session(MutationRequestId::from_bytes([0x41; 16]), None)
            .await
            .expect("session should be created");
        let accepted = store
            .accept_session_input(
                MutationRequestId::from_bytes([0x42; 16]),
                session.id,
                "recover me".to_owned(),
                model_selection(),
            )
            .await
            .expect("input should be accepted");
        store
            .activate_run(accepted.run.id)
            .await
            .expect("run should activate");
        let context = store
            .load_run_context(accepted.run.id)
            .await
            .expect("run context should load");
        let operation_id = match store
            .prepare_provider_operation(
                accepted.run.id,
                context.current_entry_high_water,
                context.estimated_input_tokens,
            )
            .await
            .expect("provider operation should prepare")
        {
            PrepareOperationOutcome::Prepared(operation_id) => operation_id,
            other => panic!("unexpected preparation outcome: {other:?}"),
        };
        store
            .mark_provider_dispatched(accepted.run.id, operation_id)
            .await
            .expect("provider operation should be marked dispatched");
        run_id = accepted.run.id;
        session_id = session.id;
    }

    let reopened = SessionStore::open_at(root.path()).expect("session store should recover");
    let recovered = reopened
        .get_run(session_id, run_id)
        .await
        .expect("recovered run query should succeed")
        .expect("recovered run should exist");
    assert_eq!(recovered.state, RunState::Interrupted);
    reopened
        .accept_session_input(
            MutationRequestId::from_bytes([0x43; 16]),
            session_id,
            "new run after recovery".to_owned(),
            model_selection(),
        )
        .await
        .expect("interrupted provider usage should not block new input");
}

#[tokio::test(flavor = "current_thread")]
async fn run_projections_rebuild_and_canonical_corruption_fails_closed() {
    let root = TestRoot::new("run-projection-repair");
    let run_id;
    let session_id;
    {
        let store = SessionStore::open_at(root.path()).expect("session store should open");
        configure_credential(&store).await;
        let session = store
            .create_session(MutationRequestId::from_bytes([0x51; 16]), None)
            .await
            .expect("session should be created");
        let accepted = store
            .accept_session_input(
                MutationRequestId::from_bytes([0x52; 16]),
                session.id,
                "repair projections".to_owned(),
                model_selection(),
            )
            .await
            .expect("input should be accepted");
        store
            .activate_run(accepted.run.id)
            .await
            .expect("run should activate");
        run_id = accepted.run.id;
        session_id = session.id;
    }

    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection = Connection::open(&database_path).expect("database should open for damage");
    connection
        .execute_batch("PRAGMA foreign_keys = OFF")
        .expect("test should disable foreign-key enforcement");
    connection
        .execute(
            "UPDATE session_run_states SET active_run_id = zeroblob(16)",
            [],
        )
        .expect("session state projection should accept test-only foreign-key damage");
    connection
        .execute("DELETE FROM runs", [])
        .expect("run projection should be removable");
    connection
        .execute("DELETE FROM delivery_events WHERE event_kind != 1", [])
        .expect("delivery projections should be removable");
    drop(connection);

    let repaired = SessionStore::open_at(root.path()).expect("projections should rebuild");
    let recovered = repaired
        .get_run(session_id, run_id)
        .await
        .expect("run query should succeed")
        .expect("run should remain present");
    assert_eq!(recovered.state, RunState::Interrupted);
    drop(repaired);

    let connection = Connection::open(&database_path).expect("database should open for corruption");
    connection
        .execute(
            "UPDATE run_input_requests SET operation_fingerprint = zeroblob(32)",
            [],
        )
        .expect("canonical fingerprint should be corruptible for test");
    drop(connection);
    let error = match SessionStore::open_at(root.path()) {
        Ok(store) => {
            drop(store);
            panic!("canonical corruption should fail");
        }
        Err(error) => error,
    };
    assert!(matches!(error, PersistenceError::InvalidState { .. }));
}
