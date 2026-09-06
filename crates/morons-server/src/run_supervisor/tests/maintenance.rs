mod lifecycle;
mod measurements;
mod tool_results;

use super::*;
use crate::maintenance_supervisor::MaintenanceSupervisor;
use crate::persistence::{
    RunId as StoredRunId, SessionId as StoredSessionId,
    maintenance::{MaintenanceResult, MaintenanceState},
};

pub(crate) async fn eligible(
    enabled: bool,
) -> (
    TestRoot,
    TestRoot,
    Arc<SessionStore>,
    StoredSessionId,
    StoredRunId,
) {
    let (root, selected, store, session) = hardening::fixture("background-runtime").await;
    drop(store);
    let store = if enabled {
        SessionStore::open_for_test_with_maintenance(root.path())
    } else {
        SessionStore::open_for_test(root.path())
    }
    .unwrap();
    for index in 1..=5 {
        append_completed_context_run(&store, session, index, &format!("PREFIX_{index}"), 11_000)
            .await;
    }
    let connection = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    connection.execute("UPDATE provider_operation_facts SET input_tokens = 60000, total_tokens = 60010 WHERE fact_kind = 3", []).unwrap();
    let run = connection
        .query_row(
            "SELECT run_id FROM run_accepted_facts ORDER BY fact_sequence DESC LIMIT 1",
            [],
            |row| row.get::<_, [u8; 16]>(0),
        )
        .unwrap();
    (
        root,
        selected,
        Arc::new(store),
        session,
        StoredRunId::from_bytes(run),
    )
}

fn lower_advisory_usage(root: &TestRoot) {
    let connection = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    connection.execute("UPDATE provider_operation_facts SET input_tokens = 10, total_tokens = 20 WHERE fact_kind = 3", []).unwrap();
}

fn summary() -> MaintenanceResult {
    MaintenanceResult {
        summary: "BOUND_BACKGROUND_SUMMARY".to_owned(),
        input_tokens: 100,
        cached_input_tokens: 20,
        cache_write_input_tokens: 0,
        output_tokens: 10,
        reasoning_output_tokens: 0,
        total_tokens: 110,
    }
}

pub(crate) async fn wait_state(
    store: &SessionStore,
    session: StoredSessionId,
    expected: MaintenanceState,
) {
    time::timeout(TERMINAL_RUN_TEST_TIMEOUT, async {
        loop {
            let status = store
                .session_context_status(session, hardening::selection())
                .await
                .unwrap();
            if status
                .background_compaction
                .latest
                .as_ref()
                .is_some_and(|job| job.state == expected)
            {
                return;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("maintenance should reach expected state");
}

async fn next_run(
    store: &SessionStore,
    session: StoredSessionId,
    text: &str,
    index: u8,
) -> StoredRunId {
    let run = store
        .accept_session_input(
            PersistenceMutationRequestId::from_bytes([index; 16]),
            session,
            text.to_owned(),
            hardening::selection(),
        )
        .await
        .unwrap()
        .run
        .id;
    store.activate_run(run).await.unwrap();
    run
}

#[tokio::test(flavor = "current_thread")]
async fn maintenance_prepares_before_hard_capacity_with_low_reported_tokens() {
    let (root, _selected, store, session, _) = eligible(true).await;
    lower_advisory_usage(&root);
    for index in 6..=7 {
        append_completed_context_run(&store, session, index, "MORE_SOURCE", 15_000).await;
    }
    let page = store
        .list_session_transcript_window(
            session,
            None,
            crate::persistence::TranscriptPageDirection::Older,
            1,
        )
        .await
        .unwrap();
    let crate::persistence::TranscriptEntry::AssistantMessage { run_id, .. } =
        page.entries.last().unwrap()
    else {
        panic!("assistant required")
    };
    let status = store
        .session_context_status(session, hardening::selection())
        .await
        .unwrap();
    assert!(status.estimate_uses_provider_usage);
    assert!(status.estimated_input_tokens < 58_800);
    assert!(status.conservative_input_tokens < status.maximum_input_tokens);
    assert!(store.prepare_maintenance(*run_id).await.unwrap().is_some());
    store
        .finish_maintenance(session, false, false)
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn disabled_maintenance_skips_and_new_input_cancels_undispatched_preparation() {
    for enabled in [false, true] {
        let (_root, _selected, store, session, trigger) = eligible(enabled).await;
        let prepared = store.prepare_maintenance(trigger).await.unwrap();
        assert_eq!(prepared.is_some(), enabled);
        if let Some(work) = prepared {
            assert!(store.prepare_maintenance(trigger).await.unwrap().is_none());
            let run = next_run(&store, session, "NEW_INPUT", 6).await;
            assert!(!store.dispatch_maintenance(work.id).await.unwrap());
            wait_state(&store, session, MaintenanceState::Cancelled).await;
            store.finish_run_stopped(run, None).await.unwrap();
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn ready_installation_is_atomic_source_bound_and_prepared_requests_are_immutable() {
    let (root, _selected, store, session, trigger) = eligible(true).await;
    let work = store.prepare_maintenance(trigger).await.unwrap().unwrap();
    assert!(store.dispatch_maintenance(work.id).await.unwrap());
    lower_advisory_usage(&root);
    let run = next_run(&store, session, "CURRENT_INPUT", 6).await;
    let before = store.load_run_context(run).await.unwrap();
    let old_request = build_provider_request(&before, None).unwrap();
    let old_bytes = old_request.encoded_body();
    store
        .complete_maintenance(work.id, summary())
        .await
        .unwrap();
    assert!(store.maintenance_boundary(run).await.unwrap().is_none());
    wait_state(&store, session, MaintenanceState::Installed).await;
    let context = store.load_run_context(run).await.unwrap();
    let checkpoint = context.checkpoint.as_ref().unwrap();
    assert_eq!(
        checkpoint.source_entry_high_water,
        work.plan.source_entry_high_water
    );
    let new_bytes = build_provider_request(&context, None)
        .unwrap()
        .encoded_body();
    assert_eq!(old_request.encoded_body(), old_bytes);
    assert!(!String::from_utf8_lossy(&old_bytes).contains("BOUND_BACKGROUND_SUMMARY"));
    assert!(String::from_utf8_lossy(&new_bytes).contains("BOUND_BACKGROUND_SUMMARY"));
    assert!(String::from_utf8_lossy(&new_bytes).contains("CURRENT_INPUT"));
    assert!(!String::from_utf8_lossy(&new_bytes).contains("PREFIX_1"));
    store.maintenance_boundary(run).await.unwrap();
    store.finish_run_stopped(run, None).await.unwrap();
    drop(store);
    SessionStore::open_for_test_with_maintenance(root.path()).unwrap();
    let connection = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM context_checkpoints", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    connection.execute("UPDATE compaction_maintenance_events SET installed_entry_high_water = 1 WHERE state = 8", []).unwrap();
    assert!(SessionStore::open_for_test_with_maintenance(root.path()).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn late_readiness_waits_for_another_run_and_manual_input_never_consumes_it() {
    let (root, _selected, store, session, trigger) = eligible(true).await;
    let work = store.prepare_maintenance(trigger).await.unwrap().unwrap();
    assert!(store.dispatch_maintenance(work.id).await.unwrap());
    lower_advisory_usage(&root);
    let run = next_run(&store, session, "FIRST", 6).await;
    let context = store.load_run_context(run).await.unwrap();
    let PrepareOperationOutcome::Prepared(operation) = store
        .prepare_provider_operation(
            run,
            context.current_entry_high_water,
            context.estimated_input_tokens,
        )
        .await
        .unwrap()
    else {
        panic!("prepared operation required");
    };
    store
        .complete_maintenance(work.id, summary())
        .await
        .unwrap();
    store.maintenance_boundary(run).await.unwrap();
    wait_state(&store, session, MaintenanceState::Ready).await;
    assert!(
        store
            .load_run_context(run)
            .await
            .unwrap()
            .checkpoint
            .is_none()
    );
    store
        .finish_run_stopped(run, Some(operation))
        .await
        .unwrap();
    let manual = next_run(&store, session, "/compact preserve new guidance", 7).await;
    assert_eq!(
        store.maintenance_boundary(manual).await.unwrap(),
        Some(session)
    );
    store
        .finish_maintenance(session, false, true)
        .await
        .unwrap();
    let context = store.load_run_context(manual).await.unwrap();
    assert!(context.checkpoint.is_none());
    assert_eq!(
        context.compaction_plan.unwrap().user_guidance.as_deref(),
        Some("preserve new guidance")
    );
    wait_state(&store, session, MaintenanceState::Discarded).await;
    store.finish_run_stopped(manual, None).await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn ready_results_are_discarded_for_new_guidance_or_disabled_startup() {
    for disabled in [false, true] {
        let (root, _selected, store, session, trigger) = eligible(true).await;
        let work = store.prepare_maintenance(trigger).await.unwrap().unwrap();
        assert!(store.dispatch_maintenance(work.id).await.unwrap());
        store
            .complete_maintenance(work.id, summary())
            .await
            .unwrap();
        if disabled {
            drop(store);
            let store = SessionStore::open_for_test(root.path()).unwrap();
            wait_state(&store, session, MaintenanceState::Discarded).await;
            assert!(!store.background_compaction_enabled());
        } else {
            let accepted = store
                .accept_session_input_with_context(
                    PersistenceMutationRequestId::from_bytes([6; 16]),
                    session,
                    "NEW_INPUT".to_owned(),
                    hardening::selection(),
                    crate::persistence::RunInputContext {
                        project: crate::project_context::RunProjectContext {
                            enabled: false,
                            ..Default::default()
                        },
                        skills: Default::default(),
                        attachments: Vec::new(),
                    },
                )
                .await
                .unwrap();
            store.activate_run(accepted.run.id).await.unwrap();
            store.maintenance_boundary(accepted.run.id).await.unwrap();
            wait_state(&store, session, MaintenanceState::Discarded).await;
            store
                .finish_run_stopped(accepted.run.id, None)
                .await
                .unwrap();
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn supervised_request_is_single_tools_free_and_cancellable_without_blocking_input() {
    let (_root, selected, store, session, trigger) = eligible(true).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let provider = Arc::new(OpenCodeProvider::for_test(Arc::clone(&store), &base));
    let shutdown = tokio::sync::watch::channel(false).0;
    let supervisor = MaintenanceSupervisor::new(Arc::clone(&store), provider, shutdown.clone());
    let (observed_tx, observed) = oneshot::channel();
    let peer = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = String::from_utf8(read_http_request(&mut stream).await).unwrap();
        observed_tx.send(request).unwrap();
        let mut byte = [0; 1];
        assert_eq!(
            time::timeout(TERMINAL_RUN_TEST_TIMEOUT, stream.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        assert!(
            time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    });
    supervisor.maybe_start(session, trigger).await;
    let raw = time::timeout(TERMINAL_RUN_TEST_TIMEOUT, observed)
        .await
        .unwrap()
        .unwrap();
    let body = request_body(&raw);
    assert_eq!(body["tools"].as_array().unwrap().len(), 0);
    assert!(raw.contains("Summarize the supplied earlier session prefix"));
    supervisor.maybe_start(session, trigger).await;
    let other = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0xd0; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .unwrap()
        .id;
    append_completed_context_run(&store, other, 0xd1, "OTHER_SESSION", 0).await;
    let page = store
        .list_session_transcript_window(
            other,
            None,
            crate::persistence::TranscriptPageDirection::Older,
            1,
        )
        .await
        .unwrap();
    let crate::persistence::TranscriptEntry::AssistantMessage {
        run_id: other_run, ..
    } = page.entries.last().unwrap()
    else {
        panic!("assistant entry required");
    };
    time::timeout(
        Duration::from_millis(250),
        supervisor.maybe_start(other, *other_run),
    )
    .await
    .expect("busy maintenance admission must not queue");
    supervisor.cancel_session(other).await.unwrap();
    wait_state(&store, session, MaintenanceState::Dispatched).await;
    let run = time::timeout(
        Duration::from_secs(5),
        next_run(&store, session, "WHILE_BACKGROUND", 6),
    )
    .await
    .unwrap();
    supervisor.cancel_session(session).await.unwrap();
    wait_state(&store, session, MaintenanceState::Uncertain).await;
    peer.await.unwrap();
    assert!(!*shutdown.borrow());
    store.finish_run_stopped(run, None).await.unwrap();
    supervisor.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn ready_summary_cannot_bypass_the_receiving_tail_budget() {
    for bytes in [30_000, 60_000] {
        let (_root, _selected, store, session, trigger) = eligible(true).await;
        let work = store.prepare_maintenance(trigger).await.unwrap().unwrap();
        assert!(store.dispatch_maintenance(work.id).await.unwrap());
        store
            .complete_maintenance(work.id, summary())
            .await
            .unwrap();
        let run = next_run(&store, session, &"x".repeat(bytes), 6).await;
        store.maintenance_boundary(run).await.unwrap();
        wait_state(&store, session, MaintenanceState::Discarded).await;
        let context = store.load_run_context(run).await.unwrap();
        assert!(context.checkpoint.is_none());
        assert!(
            context.compaction_plan.unwrap().source_entry_high_water
                > work.plan.source_entry_high_water
        );
        store.finish_run_stopped(run, None).await.unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn deadline_and_shutdown_close_blocked_requests_without_replay() {
    for stop in [false, true] {
        let (_root, _selected, store, session, trigger) = eligible(true).await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let provider = Arc::new(OpenCodeProvider::for_test(Arc::clone(&store), &base));
        let supervisor = MaintenanceSupervisor::with_deadline_for_test(
            Arc::clone(&store),
            provider,
            tokio::sync::watch::channel(false).0,
            Duration::from_secs(2),
        );
        let (sent, received) = oneshot::channel();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http_request(&mut stream).await;
            sent.send(()).unwrap();
            let mut byte = [0; 1];
            assert_eq!(
                time::timeout(TERMINAL_RUN_TEST_TIMEOUT, stream.read(&mut byte))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
        });
        supervisor.maybe_start(session, trigger).await;
        time::timeout(TERMINAL_RUN_TEST_TIMEOUT, received)
            .await
            .unwrap()
            .unwrap();
        if stop {
            supervisor.shutdown().await;
        }
        wait_state(&store, session, MaintenanceState::Uncertain).await;
        peer.await.unwrap();
        supervisor.shutdown().await;
        assert!(store.prepare_maintenance(trigger).await.unwrap().is_none());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn successful_root_triggers_distinct_maintenance_and_next_request_installs_it() {
    let (root, _selected, store, session, _trigger) = eligible(true).await;
    lower_advisory_usage(&root);
    let store = Arc::try_unwrap(store).ok().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (done_tx, done) = oneshot::channel();
    let peer = tokio::spawn(async move {
        let output = r#"{"id":"msg","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"done","annotations":[]}]}"#;
        let (mut root, _) = listener.accept().await.unwrap();
        let root_request = String::from_utf8(read_http_request(&mut root).await).unwrap();
        assert!(
            !request_body(&root_request)["tools"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let body = provider_output_body("root_response", output)
            .replace("\"input_tokens\":8", "\"input_tokens\":60000")
            .replace("\"total_tokens\":11", "\"total_tokens\":60003");
        write_provider_headers(&mut root, body.len()).await;
        root.write_all(body.as_bytes()).await.unwrap();
        root.shutdown().await.unwrap();
        let (mut background, _) = listener.accept().await.unwrap();
        let background_request =
            String::from_utf8(read_http_request(&mut background).await).unwrap();
        assert_ne!(
            request_header(&root_request, "x-opencode-session"),
            request_header(&background_request, "x-opencode-session")
        );
        assert!(
            request_body(&background_request)["tools"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        write_provider_output(
            &mut background,
            "background_response",
            &output.replace("done", "BACKGROUND_BOUNDARY_SUMMARY"),
        )
        .await;
        done_tx.send(()).unwrap();
        let (mut next, _) = listener.accept().await.unwrap();
        let next_request = String::from_utf8(read_http_request(&mut next).await).unwrap();
        assert_eq!(
            request_header(&root_request, "x-opencode-session"),
            request_header(&next_request, "x-opencode-session")
        );
        assert!(
            !request_body(&next_request)["tools"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(next_request.contains("BACKGROUND_BOUNDARY_SUMMARY"));
        assert!(next_request.contains("NEXT_INPUT"));
        assert!(!next_request.contains("PREFIX_1"));
        write_provider_output(&mut next, "next_response", output).await;
    });
    let application = ServerApplication::from_session_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.as_bytes());
    let accepted = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([6; 16]),
            session_id,
            text: "Continue".to_owned(),
            attachments: Vec::new(),
            service: OpenCodeService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .unwrap();
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        accepted
    else {
        panic!("run required");
    };
    assert_eq!(
        wait_for_terminal(&application, session_id, run.id).await,
        RunState::Succeeded
    );
    time::timeout(TERMINAL_RUN_TEST_TIMEOUT, done)
        .await
        .unwrap()
        .unwrap();
    time::timeout(TERMINAL_RUN_TEST_TIMEOUT, async {
        loop {
            let state = application
                .execute_for_local_owner(ApplicationRequest::GetSessionContext {
                    session_id,
                    service: OpenCodeService::Zen,
                    model_id: "muse-spark-1.2".to_owned(),
                })
                .await
                .unwrap();
            if let ApplicationOutcome::Response(ApplicationResponse::SessionContextFound {
                context,
            }) = state
                && context.background_compaction.latest.is_some_and(|job| {
                    job.state == morons_protocol::BackgroundCompactionState::Ready
                })
            {
                break;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let accepted = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([7; 16]),
            session_id,
            text: "NEXT_INPUT".to_owned(),
            attachments: Vec::new(),
            service: OpenCodeService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .unwrap();
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        accepted
    else {
        panic!("run required")
    };
    assert_eq!(
        wait_for_terminal(&application, session_id, run.id).await,
        RunState::Succeeded
    );
    peer.await.unwrap();
    let ApplicationOutcome::Response(ApplicationResponse::SessionContextFound { context }) =
        application
            .execute_for_local_owner(ApplicationRequest::GetSessionContext {
                session_id,
                service: OpenCodeService::Zen,
                model_id: "muse-spark-1.2".to_owned(),
            })
            .await
            .unwrap()
    else {
        panic!("context required")
    };
    assert_eq!(
        context.background_compaction.latest.unwrap().state,
        morons_protocol::BackgroundCompactionState::Installed
    );
    assert_eq!(context.completed_compactions, 0);
    application.shutdown().await;
}
