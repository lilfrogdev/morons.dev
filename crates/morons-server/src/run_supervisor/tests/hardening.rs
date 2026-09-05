use super::*;

pub(super) async fn fixture(
    label: &str,
) -> (
    TestRoot,
    TestRoot,
    SessionStore,
    crate::persistence::SessionId,
) {
    let root = TestRoot::new(label);
    let selected = TestRoot::new(&format!("{label}-directory"));
    let store = SessionStore::open_for_test(root.path()).unwrap();
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0xe0; 16]),
            0,
            b"not-a-real-hardening-key".to_vec(),
        )
        .await
        .unwrap();
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0xe1; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .unwrap();
    (root, selected, store, session.id)
}

#[tokio::test(flavor = "current_thread")]
async fn image_pressure_compacts_before_loading_an_oversized_image_context() {
    use sha2::{Digest as _, Sha256};
    let (_root, _selected, store, session) = fixture("image-pressure").await;
    let image = morons_image::normalize_rgba(2, 2, vec![0x44; 16]).unwrap();
    let mut text = String::new();
    let mut attachments = Vec::new();
    for index in 0..4 {
        let display_name = format!("image-{index}.png");
        let marker_start = text.len() as u32;
        text.push_str(&format!("[{display_name}] "));
        attachments.push(crate::persistence::PreparedImageAttachment {
            display_name,
            marker_start,
            media_type: image.media_type,
            width: image.width,
            height: image.height,
            digest: Sha256::digest(&image.bytes).into(),
            bytes: image.bytes.clone(),
        });
    }
    let mut model = selection();
    model.model_id = "gpt-5-nano".to_owned();
    model.supports_image_input = true;
    for index in 1..=4 {
        let run = store
            .accept_session_input_with_skills(
                PersistenceMutationRequestId::from_bytes([index; 16]),
                session,
                text.clone(),
                model.clone(),
                crate::skills::RunSkillContext::default(),
                attachments.clone(),
            )
            .await
            .unwrap();
        store.finish_run_stopped(run.run.id, None).await.unwrap();
    }
    let accepted = store
        .accept_session_input_with_skills(
            PersistenceMutationRequestId::from_bytes([5; 16]),
            session,
            text,
            model,
            crate::skills::RunSkillContext::default(),
            attachments,
        )
        .await
        .expect("old image capacity must permit compaction admission");
    store.activate_run(accepted.run.id).await.unwrap();
    let mut context = store.load_run_context(accepted.run.id).await.unwrap();
    assert!(context.attachment_data.is_empty());
    let plan = context
        .compaction_plan
        .take()
        .expect("twenty images require compaction before raw bytes load");
    assert_eq!(plan.source_entry_high_water, 3);
    assert!(plan.source.contains("IMAGE:"));
    let operation = store
        .prepare_auto_compaction(accepted.run.id, &plan)
        .await
        .unwrap();
    store
        .mark_compaction_dispatched(accepted.run.id, operation)
        .await
        .unwrap();
    store
        .complete_compaction(
            accepted.run.id,
            operation,
            RunOpenCodeService::Zen,
            "gpt-5-nano".to_owned(),
            "Earlier images summarized.".to_owned(),
        )
        .await
        .unwrap();
    let context = store.load_run_context(accepted.run.id).await.unwrap();
    assert!(context.compaction_plan.is_none());
    assert_eq!(context.attachment_data.len(), 8);
    build_provider_request(&context, None)
        .expect("retained image context must fit the wire request");
    store
        .finish_run_stopped(accepted.run.id, None)
        .await
        .unwrap();
}

pub(super) fn selection() -> RunModelSelection {
    RunModelSelection {
        service: RunOpenCodeService::Zen,
        model_id: "muse-spark-1.2".to_owned(),
        protocol_revision: 1,
        maximum_input_tokens: 96_000,
        maximum_output_tokens: 32_000,
        supports_tool_calls: true,
        supports_image_input: false,
    }
}

async fn submit(
    application: &ServerApplication,
    session_id: SessionId,
    text: String,
) -> morons_protocol::RunSummary {
    let accepted = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0xe2; 16]),
            session_id,
            text,
            attachments: Vec::new(),
            service: OpenCodeService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .expect("recovery input must remain acceptable");
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        accepted
    else {
        panic!("run required")
    };
    run
}

#[tokio::test(flavor = "current_thread")]
async fn entry_pressure_compacts_automatically_and_manual_recovery_works_at_capacity() {
    for (turns, prompt) in [(84_u8, "continue"), (128_u8, "/compact")] {
        let (_root, _selected, store, session) = fixture("entry-pressure").await;
        // Populate canonical history through the storage fixture, deliberately
        // bypassing automatic compaction to also reproduce older full sessions.
        for index in 1..=turns {
            append_completed_context_run(&store, session, index, &format!("SHORT_{index:03}"), 0)
                .await;
        }
        let status = store
            .session_context_status(session, selection())
            .await
            .unwrap();
        assert!(status.estimated_input_tokens < status.compaction_threshold_tokens);
        let (base, requests, provider_task) = spawn_compaction_provider().await;
        let application = ServerApplication::from_session_store_for_test(store, &base);
        let session_id = SessionId::from_bytes(*session.as_bytes());
        let run = submit(&application, session_id, prompt.to_owned()).await;
        let terminal = wait_for_terminal(&application, session_id, run.id).await;
        application.shutdown().await;
        assert_eq!(terminal, RunState::Succeeded);
        provider_task.await.unwrap();
        let requests = requests.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].contains("Summarize the supplied earlier session prefix"));
        assert!(requests[1].contains("SUMMARY_OF_OLDEST_PREFIX"));
        assert!(!requests[1].contains("SHORT_001"));
        assert!(requests[1].contains(&format!("SHORT_{turns:03}")));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn context_overflow_after_a_completed_read_is_durably_terminal() {
    let (_root, selected, store, session) = fixture("context-overflow").await;
    fs::write(selected.path().join("note.txt"), "x".repeat(60_000)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = read_http_request(&mut stream).await;
        let arguments = serde_json::to_string(r#"{"path":"note.txt"}"#).unwrap();
        let output = format!(
            r#"{{"id":"fc_overflow","type":"function_call","status":"completed","call_id":"overflow_read","name":"read","arguments":{arguments}}}"#
        );
        write_provider_output(&mut stream, "overflow_response", &output).await;
    });
    let application = ServerApplication::from_session_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.as_bytes());
    let run = submit(&application, session_id, "a".repeat(40_000)).await;
    let terminal = wait_for_terminal(&application, session_id, run.id).await;
    let snapshot = application
        .execute_for_local_owner(ApplicationRequest::GetRun {
            session_id,
            run_id: run.id,
        })
        .await
        .unwrap();
    application.shutdown().await;
    server.await.unwrap();
    assert_eq!(terminal, RunState::Failed);
    let ApplicationOutcome::Response(ApplicationResponse::RunFound { run }) = snapshot else {
        panic!("run required")
    };
    assert_eq!(
        run.failure,
        Some(morons_protocol::RunFailureKind::ResourceLimit)
    );
    assert_eq!(
        fs::read(selected.path().join("note.txt")).unwrap().len(),
        60_000
    );
}

#[tokio::test(flavor = "current_thread")]
async fn oversized_prefix_uses_disclosed_bounded_excerpts_and_external_corruption_fails_closed() {
    let (root, _selected, store, session) = fixture("bounded-compaction").await;
    append_completed_context_run(&store, session, 1, "OLD_LARGE_SOURCE", 120_000).await;
    let hidden = store
        .accept_local_command(
            PersistenceMutationRequestId::from_bytes([0xe3; 16]),
            session,
            "HIDDEN_COMMAND_NEVER_SENT".to_owned(),
            false,
        )
        .await
        .unwrap();
    assert!(store.activate_local_command(hidden.id).await.unwrap());
    store
        .complete_local_command(
            hidden.id,
            crate::tools::ToolResult::Ok {
                output: crate::tools::ToolOutput::Bash {
                    exit_code: Some(0),
                    signal: None,
                    stdout: "HIDDEN_OUTPUT_NEVER_SENT".to_owned(),
                    stderr: String::new(),
                },
            },
        )
        .await
        .unwrap();
    let (base, requests, provider_task) = spawn_compaction_provider().await;
    let application = ServerApplication::from_session_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.as_bytes());
    let run = submit(
        &application,
        session_id,
        "/compact preserve the current goal".to_owned(),
    )
    .await;
    assert_eq!(
        wait_for_terminal(&application, session_id, run.id).await,
        RunState::Succeeded
    );
    provider_task.await.unwrap();
    let requests = requests.await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("Source excerpt truncated"));
    assert!(requests[0].contains("OLD_LARGE_SOURCE"));
    assert!(requests[0].len() < 64 * 1024);
    assert!(!requests.iter().any(|request| request.contains("HIDDEN_")));
    // Canonical content was not truncated and the external writer invalidates
    // the worker's integrity proof even though the connection stays open.
    let connection = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    let bytes: i64 = connection
        .query_row("SELECT MAX(length(text)) FROM session_entries", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(bytes > 120_000);
    connection
        .execute(
            "UPDATE context_checkpoints SET source_digest = zeroblob(32)",
            [],
        )
        .unwrap();
    drop(connection);
    let status = application
        .execute_for_local_owner(ApplicationRequest::GetSessionContext {
            session_id,
            service: OpenCodeService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await;
    assert!(matches!(status, Err(ApplicationError::Internal)));
    application.shutdown().await;
    drop(application);
    assert!(SessionStore::open_for_test(root.path()).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn compaction_rejection_or_oversized_summary_fails_once_without_installing_a_checkpoint() {
    for oversized_summary in [false, true] {
        let (root, _selected, store, session) = fixture("failed-compaction").await;
        append_completed_context_run(&store, session, 1, "OLD_SOURCE", 0).await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut stream).await;
            assert!(
                String::from_utf8(request)
                    .unwrap()
                    .contains("Summarize the supplied earlier session prefix")
            );
            if oversized_summary {
                let output = serde_json::json!({"id":"summary_message","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"s".repeat(20_000),"annotations":[]}]}).to_string();
                write_provider_output(&mut stream, "oversized_summary", &output).await;
            } else {
                stream.write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                stream.shutdown().await.unwrap();
            }
        });
        let application = ServerApplication::from_session_store_for_test(store, &base);
        let session_id = SessionId::from_bytes(*session.as_bytes());
        let run = submit(&application, session_id, "/compact".to_owned()).await;
        assert_eq!(
            wait_for_terminal(&application, session_id, run.id).await,
            RunState::Failed
        );
        server.await.unwrap();
        assert!(!*application.subscribe_shutdown_requests().borrow());
        application.shutdown().await;
        let connection =
            rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM context_checkpoints", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM compaction_operations WHERE state = 5",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_compaction_closes_the_request_and_commits_cancellation() {
    let (root, _selected, store, session) = fixture("cancel-compaction").await;
    append_completed_context_run(&store, session, 1, "OLD_SOURCE", 0).await;
    let (base, dispatched, server) = spawn_stalled_provider().await;
    let application = ServerApplication::from_session_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.as_bytes());
    let run = submit(&application, session_id, "/compact".to_owned()).await;
    time::timeout(TERMINAL_RUN_TEST_TIMEOUT, dispatched)
        .await
        .unwrap()
        .unwrap();
    application
        .execute_for_local_owner(ApplicationRequest::CancelRun {
            mutation_request_id: MutationRequestId::from_bytes([0xe4; 16]),
            session_id,
            run_id: run.id,
        })
        .await
        .unwrap();
    assert_eq!(
        wait_for_terminal(&application, session_id, run.id).await,
        RunState::Cancelled
    );
    time::timeout(TERMINAL_RUN_TEST_TIMEOUT, server)
        .await
        .unwrap()
        .unwrap();
    application.shutdown().await;
    let connection = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM context_checkpoints", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn unexpected_integrity_failure_requests_shutdown_instead_of_abandoning_admission() {
    let (root, _selected, store, session) = fixture("fatal-context").await;
    let accepted = store
        .accept_session_input(
            PersistenceMutationRequestId::from_bytes([1; 16]),
            session,
            "hello".to_owned(),
            selection(),
        )
        .await
        .unwrap();
    let connection = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    connection
        .execute("DELETE FROM session_entries", [])
        .unwrap();
    drop(connection);
    let store = Arc::new(store);
    let provider = Arc::new(OpenCodeProvider::for_test(
        Arc::clone(&store),
        "http://127.0.0.1:9",
    ));
    let supervisor = RunSupervisor::new(store, provider, SessionEventHub::new());
    let mut shutdown = supervisor.shutdown_requests().subscribe();
    supervisor
        .start(accepted.run.id, supervisor.try_reserve().unwrap())
        .await
        .unwrap();
    time::timeout(TERMINAL_RUN_TEST_TIMEOUT, shutdown.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(*shutdown.borrow());
    assert!(supervisor.try_reserve().is_none());
    supervisor.shutdown().await;
}
