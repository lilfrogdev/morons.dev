use std::{fs, path::PathBuf, process, sync::Arc};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use rusqlite::Connection;

use super::{
    ActivationOutcome, CompletedAssistant, CompletedToolTurn, DispatchOutcome, MutationRequestId,
    OpenCodeCredentialStatus, PersistenceError, PrepareOperationOutcome, ProviderUsage,
    RunModelSelection, RunOpenCodeService, RunState, SessionEventCursor, SessionEventPayload,
    SessionStore, TranscriptCursor, TranscriptEntry,
};
use crate::tools::{
    ToolErrorKind, ToolExecution, ToolInput, ToolResult, ValidatedProviderCall, WorktreePath,
    WorktreeToolExecutor,
};

const TEST_MODEL: &str = "muse-spark-1.2";

#[tokio::test(flavor = "current_thread")]
async fn rejected_run_input_does_not_append_transcript_state() {
    let root = TestRoot::new("rejected-run-input");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let session = store
        .create_session(MutationRequestId::from_bytes([0x09; 16]), None)
        .await
        .expect("session should be created");
    let error = store
        .accept_session_input(
            MutationRequestId::from_bytes([0x0a; 16]),
            session.id,
            "must not commit".to_owned(),
            model_selection(),
        )
        .await
        .expect_err("missing credential should reject input");
    assert!(matches!(error, PersistenceError::CredentialNotConfigured));
    let transcript = store
        .list_session_transcript(session.id, None, 1)
        .await
        .expect("empty transcript should remain readable");
    assert!(transcript.entries.is_empty());
    assert!(transcript.next_cursor.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn run_input_is_atomic_idempotent_and_session_serialized() {
    let root = TestRoot::new("run-acceptance");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    configure_credential(&store).await;
    let session = store
        .create_session(MutationRequestId::from_bytes([0x11; 16]), None)
        .await
        .expect("session should be created");
    let request_id = MutationRequestId::from_bytes([0x12; 16]);
    let accepted = store
        .accept_session_input(
            request_id,
            session.id,
            "hello durable run".to_owned(),
            model_selection(),
        )
        .await
        .expect("input should be accepted");

    assert!(accepted.newly_accepted);
    assert_eq!(accepted.run.state, RunState::Accepted);
    assert_eq!(accepted.run.credential_generation, 1);
    let retry = store
        .find_session_input_retry(
            request_id,
            session.id,
            "hello durable run",
            RunOpenCodeService::Zen,
            TEST_MODEL,
        )
        .await
        .expect("retry should resolve")
        .expect("retry should exist");
    assert!(!retry.newly_accepted);
    assert_eq!(retry.run.id, accepted.run.id);
    assert_eq!(retry.user_message_id, accepted.user_message_id);

    let conflict = store
        .find_session_input_retry(
            request_id,
            session.id,
            "different input",
            RunOpenCodeService::Zen,
            TEST_MODEL,
        )
        .await
        .expect_err("conflicting retry should fail");
    assert!(matches!(conflict, PersistenceError::RequestConflict));

    let busy = store
        .accept_session_input(
            MutationRequestId::from_bytes([0x13; 16]),
            session.id,
            "must not queue".to_owned(),
            model_selection(),
        )
        .await
        .expect_err("a session with a run should be busy");
    assert!(matches!(
        busy,
        PersistenceError::SessionBusy { active_run_id } if active_run_id == accepted.run.id
    ));

    let page = store
        .list_session_transcript(session.id, None, 1)
        .await
        .expect("transcript should be readable");
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.active_run_id, Some(accepted.run.id));
    assert!(matches!(
        &page.runs[..],
        [run] if run.id == accepted.run.id && run.state == RunState::Accepted
    ));
    assert!(matches!(
        &page.entries[0],
        TranscriptEntry::UserMessage { id, run_id, text, .. }
            if *id == accepted.user_message_id
                && *run_id == accepted.run.id
                && text == "hello durable run"
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_session_input_accepts_one_run_without_queueing() {
    let root = TestRoot::new("concurrent-run-acceptance");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    configure_credential(&store).await;
    let session = store
        .create_session(MutationRequestId::from_bytes([0x61; 16]), None)
        .await
        .expect("session should be created");
    let first = store.accept_session_input(
        MutationRequestId::from_bytes([0x62; 16]),
        session.id,
        "first concurrent input".to_owned(),
        model_selection(),
    );
    let second = store.accept_session_input(
        MutationRequestId::from_bytes([0x63; 16]),
        session.id,
        "second concurrent input".to_owned(),
        model_selection(),
    );
    let (first, second) = tokio::join!(first, second);
    let outcomes = [first, second];
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(result, Err(PersistenceError::SessionBusy { .. })))
            .count(),
        1
    );
    let page = store
        .list_session_transcript(session.id, None, 1)
        .await
        .expect("transcript should remain readable");
    assert_eq!(page.entries.len(), 1);
    assert!(page.next_cursor.is_none());
}

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
async fn startup_proves_published_mutation_without_replaying_it() {
    let root = TestRoot::new("tool-recovery");
    let source = TestRoot::new("tool-recovery-source");
    fs::write(source.path().join("existing.txt"), "existing\n")
        .expect("source file should be written");
    let run_id;
    let session_id;
    let workspace_id;
    {
        let store =
            Arc::new(SessionStore::open_at(root.path()).expect("session store should open"));
        configure_credential(&store).await;
        let session = store
            .create_session(MutationRequestId::from_bytes([0x71; 16]), None)
            .await
            .expect("session should be created");
        store
            .import_repository(
                MutationRequestId::from_bytes([0x72; 16]),
                session.id,
                source.path().to_string_lossy().into_owned(),
            )
            .await
            .expect("repository should import");
        let accepted = store
            .accept_session_input(
                MutationRequestId::from_bytes([0x73; 16]),
                session.id,
                "create recovered.txt".to_owned(),
                model_selection(),
            )
            .await
            .expect("run should be accepted");
        store
            .activate_run(accepted.run.id)
            .await
            .expect("run should activate");
        let context = store
            .load_run_context(accepted.run.id)
            .await
            .expect("context should load");
        let provider_operation = match store
            .prepare_provider_operation(
                accepted.run.id,
                context.current_entry_high_water,
                context.estimated_input_tokens,
            )
            .await
            .expect("provider should prepare")
        {
            PrepareOperationOutcome::Prepared(operation) => operation,
            other => panic!("unexpected provider preparation: {other:?}"),
        };
        store
            .mark_provider_dispatched(accepted.run.id, provider_operation)
            .await
            .expect("provider should dispatch");
        let committed = store
            .complete_provider_tool_turn(
                accepted.run.id,
                provider_operation,
                CompletedToolTurn {
                    provider_response_id: "resp_recovery".to_owned(),
                    usage: ProviderUsage {
                        input_tokens: 1,
                        cached_input_tokens: 0,
                        cache_write_input_tokens: 0,
                        output_tokens: 1,
                        reasoning_output_tokens: 0,
                        total_tokens: 2,
                    },
                    commentary: None,
                    calls: vec![ValidatedProviderCall {
                        provider_call_id: "provider_recovery".to_owned(),
                        input: ToolInput::CreateFile {
                            path: WorktreePath::parse("recovered.txt", false)
                                .expect("path should parse"),
                            content: "published once\n".to_owned(),
                        },
                    }],
                },
            )
            .await
            .expect("tool call should commit");
        let call = &committed.calls[0];
        let worktree = store
            .active_worktree_path(session.workspace_id)
            .await
            .expect("active worktree should resolve");
        let ToolExecution::Mutation(prepared) = WorktreeToolExecutor::new(worktree.clone())
            .prepare(call.input.clone(), *call.operation_id.as_bytes(), &|| false)
        else {
            panic!("mutation should prepare");
        };
        let plan = prepared
            .encoded_plan()
            .expect("recovery plan should encode");
        store
            .prepare_tool_operation(accepted.run.id, call.call_id, call.operation_id, Some(plan))
            .await
            .expect("tool operation should prepare durably");
        store
            .mark_tool_dispatched(accepted.run.id, call.call_id, call.operation_id)
            .await
            .expect("tool operation should dispatch durably");
        WorktreeToolExecutor::new(worktree)
            .publish_mutation(prepared)
            .expect("mutation should publish once");
        run_id = accepted.run.id;
        session_id = session.id;
        workspace_id = session.workspace_id;
        drop(Arc::try_unwrap(store).unwrap_or_else(|_| panic!("store should be unique")));
    }

    let reopened = SessionStore::open_at(root.path()).expect("tool operation should recover");
    let run = reopened
        .get_run(session_id, run_id)
        .await
        .expect("run query should succeed")
        .expect("run should exist");
    assert_eq!(run.state, RunState::Interrupted);
    assert_eq!(
        fs::read_to_string(
            reopened
                .active_worktree_path(workspace_id)
                .await
                .expect("active worktree should resolve")
                .join("recovered.txt"),
        )
        .expect("published file should remain"),
        "published once\n"
    );
    let mut cursor = None;
    let mut saw_result = false;
    loop {
        let page = reopened
            .list_session_transcript(session_id, cursor, 1)
            .await
            .expect("transcript should page");
        saw_result |= page.entries.iter().any(|entry| {
            matches!(
                entry,
                TranscriptEntry::ToolResult {
                    result: ToolResult::Ok { .. },
                    ..
                }
            )
        });
        let Some(next) = page.next_cursor else { break };
        cursor = Some(next);
    }
    assert!(saw_result);
    drop(reopened);
    SessionStore::open_at(root.path()).expect("recovered tool result should be durable");
}

#[tokio::test(flavor = "current_thread")]
async fn uncertain_tool_effect_blocks_until_exact_acknowledgement() {
    let root = TestRoot::new("tool-uncertainty");
    let source = TestRoot::new("tool-uncertainty-source");
    fs::write(source.path().join("existing.txt"), "existing\n")
        .expect("source file should be written");
    let store = Arc::new(SessionStore::open_at(root.path()).expect("session store should open"));
    configure_credential(&store).await;
    let session = store
        .create_session(MutationRequestId::from_bytes([0x61; 16]), None)
        .await
        .expect("session should be created");
    store
        .import_repository(
            MutationRequestId::from_bytes([0x62; 16]),
            session.id,
            source.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("repository should import");
    let accepted = store
        .accept_session_input(
            MutationRequestId::from_bytes([0x63; 16]),
            session.id,
            "create uncertain.txt".to_owned(),
            model_selection(),
        )
        .await
        .expect("run should be accepted");
    assert_eq!(
        accepted.run.tool_catalog_version,
        crate::tools::TOOL_CATALOG_VERSION
    );
    assert_eq!(
        accepted.run.tool_limits_version,
        crate::tools::TOOL_LIMITS_VERSION
    );
    assert!(accepted.run.execution_image_generation.is_none());
    store
        .activate_run(accepted.run.id)
        .await
        .expect("run should activate");
    let context = store
        .load_run_context(accepted.run.id)
        .await
        .expect("context should load");
    let provider_operation = match store
        .prepare_provider_operation(
            accepted.run.id,
            context.current_entry_high_water,
            context.estimated_input_tokens,
        )
        .await
        .expect("provider operation should prepare")
    {
        PrepareOperationOutcome::Prepared(operation) => operation,
        other => panic!("unexpected provider preparation: {other:?}"),
    };
    store
        .mark_provider_dispatched(accepted.run.id, provider_operation)
        .await
        .expect("provider should dispatch");
    let committed = store
        .complete_provider_tool_turn(
            accepted.run.id,
            provider_operation,
            CompletedToolTurn {
                provider_response_id: "resp_uncertain".to_owned(),
                usage: ProviderUsage {
                    input_tokens: 1,
                    cached_input_tokens: 0,
                    cache_write_input_tokens: 0,
                    output_tokens: 1,
                    reasoning_output_tokens: 0,
                    total_tokens: 2,
                },
                commentary: None,
                calls: vec![ValidatedProviderCall {
                    provider_call_id: "provider_uncertain".to_owned(),
                    input: ToolInput::CreateFile {
                        path: WorktreePath::parse("uncertain.txt", false)
                            .expect("tool path should parse"),
                        content: "uncertain\n".to_owned(),
                    },
                }],
            },
        )
        .await
        .expect("tool call should commit");
    let call = &committed.calls[0];
    let ToolExecution::Mutation(prepared) = WorktreeToolExecutor::new(
        store
            .active_worktree_path(session.workspace_id)
            .await
            .expect("active worktree should resolve"),
    )
    .prepare(call.input.clone(), *call.operation_id.as_bytes(), &|| false) else {
        panic!("uncertain mutation should prepare");
    };
    let recovery_plan = prepared
        .encoded_plan()
        .expect("uncertain mutation plan should encode");
    drop(prepared);
    store
        .prepare_tool_operation(
            accepted.run.id,
            call.call_id,
            call.operation_id,
            Some(recovery_plan),
        )
        .await
        .expect("tool operation should prepare");
    store
        .mark_tool_dispatched(accepted.run.id, call.call_id, call.operation_id)
        .await
        .expect("tool publication should dispatch");
    store
        .complete_tool_result(
            accepted.run.id,
            call.call_id,
            call.operation_id,
            ToolResult::error(ToolErrorKind::Uncertain),
        )
        .await
        .expect("uncertainty should commit");
    let run = store
        .get_run(session.id, accepted.run.id)
        .await
        .expect("run query should succeed")
        .expect("run should exist");
    assert_eq!(run.state, RunState::Uncertain);
    let workspace = store
        .workspace_summary(session.id)
        .await
        .expect("workspace summary should load");
    assert_eq!(workspace.state, super::WorkspaceState::Blocked);
    assert_eq!(workspace.blocked_run_id, Some(accepted.run.id));
    assert!(matches!(
        store
            .accept_session_input(
                MutationRequestId::from_bytes([0x64; 16]),
                session.id,
                "must remain blocked".to_owned(),
                model_selection(),
            )
            .await,
        Err(PersistenceError::WorkspaceBlocked)
    ));

    let acknowledgement_id = MutationRequestId::from_bytes([0x65; 16]);
    let acknowledgement = store
        .acknowledge_tool_uncertainty(acknowledgement_id, session.id, accepted.run.id)
        .await
        .expect("uncertainty should acknowledge");
    assert_eq!(
        acknowledgement.workspace.state,
        super::WorkspaceState::Ready
    );
    let retry = store
        .acknowledge_tool_uncertainty(acknowledgement_id, session.id, accepted.run.id)
        .await
        .expect("acknowledgement retry should be idempotent");
    assert_eq!(retry, acknowledgement);
    store
        .accept_session_input(
            MutationRequestId::from_bytes([0x66; 16]),
            session.id,
            "continue after parking uncertainty".to_owned(),
            model_selection(),
        )
        .await
        .expect("acknowledged uncertainty should permit new input");
    drop(Arc::try_unwrap(store).unwrap_or_else(|_| panic!("store should be unique")));
    let reopened =
        SessionStore::open_at(root.path()).expect("acknowledged uncertainty should reopen");
    drop(reopened);

    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection = Connection::open(database_path).expect("database should open for corruption");
    connection
        .execute("UPDATE tool_calls SET input_payload = x'7b7d'", [])
        .expect("tool input payload should be corruptible for test");
    drop(connection);
    let error = match SessionStore::open_at(root.path()) {
        Ok(store) => {
            drop(store);
            panic!("corrupt typed tool input should fail closed");
        }
        Err(error) => error,
    };
    assert!(matches!(error, PersistenceError::InvalidState { .. }));
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

async fn configure_credential(store: &SessionStore) -> OpenCodeCredentialStatus {
    store
        .set_open_code_credential(
            MutationRequestId::from_bytes([0x01; 16]),
            0,
            b"not-a-real-run-test-key".to_vec(),
        )
        .await
        .expect("credential should be configured")
}

fn model_selection() -> RunModelSelection {
    RunModelSelection {
        service: RunOpenCodeService::Zen,
        model_id: TEST_MODEL.to_owned(),
        protocol_revision: 1,
        maximum_input_tokens: 96_000,
        maximum_output_tokens: 32_000,
        supports_tool_calls: true,
    }
}

struct TestRoot(PathBuf);

impl TestRoot {
    fn new(label: &str) -> Self {
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).expect("test randomness should be available");
        let encoded = nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path =
            std::env::temp_dir().join(format!("morons-run-{label}-{}-{encoded}", process::id()));
        fs::create_dir(&path).expect("test root should be created");
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("test root should be owner-only");
        #[cfg(windows)]
        fence_windows::harden_private_directory(&path)
            .expect("Windows test root should be hardened");
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
