use super::hardening::{fixture, selection};
use super::*;

#[tokio::test(flavor = "current_thread")]
async fn schema_25_history_migrates_without_retroactive_project_guidance() {
    let (root, selected, store, session) = fixture("project-migration").await;
    append_completed_context_run(&store, session, 1, "LEGACY", 0).await;
    drop(store);
    let connection = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    connection.execute_batch("DROP TABLE run_project_contexts;
        UPDATE run_accepted_facts SET tool_catalog_version = 8, tool_limits_version = 8;
        UPDATE runs SET tool_catalog_version = 8, tool_limits_version = 8;
        UPDATE provider_operation_facts SET tool_catalog_version = 8, tool_limits_version = 8 WHERE tool_catalog_version = 9;
        PRAGMA user_version = 25;").unwrap();
    drop(connection);
    fs::write(selected.path().join("AGENTS.md"), "NEW_GUIDANCE").unwrap();
    let store = SessionStore::open_for_test(root.path()).unwrap();
    assert!(
        store
            .session_context_status(session, selection())
            .await
            .unwrap()
            .project_context
            .is_none()
    );
    let project = crate::project_context::ProjectContextDiscovery::new()
        .discover(selected.path().to_owned())
        .await
        .unwrap();
    let accepted = store
        .accept_session_input_with_context(
            PersistenceMutationRequestId::from_bytes([2; 16]),
            session,
            "continue".to_owned(),
            selection(),
            crate::persistence::RunInputContext {
                skills: Default::default(),
                project,
                attachments: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(accepted.run.tool_catalog_version, 9);
    assert!(
        store
            .load_run_context(accepted.run.id)
            .await
            .unwrap()
            .project
            .unwrap()
            .files
            .iter()
            .any(|file| file.content == "NEW_GUIDANCE")
    );
    store
        .finish_run_stopped(accepted.run.id, None)
        .await
        .unwrap();
    drop(store);
    SessionStore::open_for_test(root.path()).unwrap();
}
use std::path::Path;

fn input(session_id: SessionId, request: u8) -> ApplicationRequest {
    ApplicationRequest::SubmitSessionInput {
        mutation_request_id: MutationRequestId::from_bytes([request; 16]),
        session_id,
        text: "Explain this project.".to_owned(),
        attachments: Vec::new(),
        service: OpenCodeService::Zen,
        model_id: "muse-spark-1.2".to_owned(),
    }
}

async fn context(
    application: &ServerApplication,
    session_id: SessionId,
) -> morons_protocol::SessionContextStatus {
    let result = application
        .execute_for_local_owner(ApplicationRequest::GetSessionContext {
            session_id,
            service: OpenCodeService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .unwrap();
    let ApplicationOutcome::Response(ApplicationResponse::SessionContextFound { context }) = result
    else {
        panic!("context response required")
    };
    context
}

async fn submit(
    application: &ServerApplication,
    session_id: SessionId,
    request: u8,
) -> morons_protocol::RunId {
    let result = application
        .execute_for_local_owner(input(session_id, request))
        .await
        .unwrap();
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        result
    else {
        panic!("run required")
    };
    run.id
}

#[tokio::test(flavor = "current_thread")]
async fn project_context_is_pinned_refreshed_retry_stable_and_usage_bound() {
    let (root, selected, store, session) = fixture("project-pinning").await;
    let source = selected.path().join("AGENTS.md");
    fs::write(&source, "ORIGINAL_GUIDANCE").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let (second_tx, second_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = tokio::spawn(async move {
        let mut requests = Vec::new();
        let (mut first, _) = listener.accept().await.unwrap();
        requests.push(read_http_request(&mut first).await);
        write_provider_output(&mut first, "resp_first", r#"{"id":"msg_first","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"first answer","annotations":[]}]}"#).await;
        let (mut second, _) = listener.accept().await.unwrap();
        requests.push(read_http_request(&mut second).await);
        second_tx.send(()).unwrap();
        release_rx.await.unwrap();
        write_provider_output(&mut second, "resp_second", r#"{"id":"msg_second","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"second answer","annotations":[]}]}"#).await;
        requests
    });
    let application = ServerApplication::from_session_store_for_test(store, &origin);
    let session_id = SessionId::from_bytes(*session.as_bytes());
    assert!(
        context(&application, session_id)
            .await
            .project_context
            .is_none()
    );
    let first = submit(&application, session_id, 1).await;
    assert_eq!(
        wait_for_terminal(&application, session_id, first).await,
        RunState::Succeeded
    );
    let before = context(&application, session_id).await;
    assert!(before.estimate_uses_provider_usage);
    assert!(
        before
            .project_context
            .as_ref()
            .unwrap()
            .files
            .iter()
            .any(|path| Path::new(path) == source)
    );
    assert!(
        !serde_json::to_string(&before)
            .unwrap()
            .contains("ORIGINAL_GUIDANCE")
    );
    fs::remove_file(&source).unwrap();
    assert_eq!(context(&application, session_id).await, before);
    assert_eq!(submit(&application, session_id, 1).await, first);
    fs::write(&source, "UPDATED_GUIDANCE").unwrap();
    let second = submit(&application, session_id, 2).await;
    time::timeout(TERMINAL_RUN_TEST_TIMEOUT, second_rx)
        .await
        .unwrap()
        .unwrap();
    let during = context(&application, session_id).await;
    assert!(!during.estimate_uses_provider_usage);
    release_tx.send(()).unwrap();
    assert_eq!(
        wait_for_terminal(&application, session_id, second).await,
        RunState::Succeeded
    );
    let requests = provider.await.unwrap();
    for (index, request) in requests.into_iter().enumerate() {
        let raw = String::from_utf8(request).unwrap();
        let body: serde_json::Value =
            serde_json::from_str(raw.split_once("\r\n\r\n").unwrap().1).unwrap();
        let entries = body["input"].as_array().unwrap();
        let project = entries
            .iter()
            .find(|entry| entry.to_string().contains("<project_context>"))
            .unwrap();
        assert_eq!(project["role"], "developer");
        assert!(project.to_string().contains(if index == 0 {
            "ORIGINAL_GUIDANCE"
        } else {
            "UPDATED_GUIDANCE"
        }));
        assert!(!project.to_string().contains(if index == 0 {
            "UPDATED_GUIDANCE"
        } else {
            "ORIGINAL_GUIDANCE"
        }));
        assert!(!entries[0].to_string().contains("GUIDANCE"));
    }
    application.shutdown().await;
    drop(application);
    fs::remove_file(&source).unwrap();
    let store = SessionStore::open_for_test(root.path()).unwrap();
    for (id, text) in [(first, "ORIGINAL_GUIDANCE"), (second, "UPDATED_GUIDANCE")] {
        let pinned = store
            .load_run_context(crate::persistence::RunId::from_bytes(*id.as_bytes()))
            .await
            .unwrap();
        assert!(
            pinned
                .project
                .unwrap()
                .files
                .iter()
                .any(|file| file.content == text)
        );
    }
    let connection = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    connection.execute("UPDATE run_project_contexts SET snapshot = replace(snapshot, 'UPDATED_GUIDANCE', 'CORRUPT_GUIDANCE') WHERE run_id = ?1", [&second.as_bytes()[..]]).unwrap();
    assert!(
        store
            .session_context_status(session, selection())
            .await
            .is_err()
    );
    drop(store);
    assert!(SessionStore::open_for_test(root.path()).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn project_context_budget_is_checked_and_snapshots_are_deleted_without_touching_sources() {
    let (root, selected, store, session) = fixture("project-budget").await;
    let source = selected.path().join("AGENTS.md");
    fs::write(&source, "x".repeat(16 * 1024)).unwrap();
    let project = crate::project_context::ProjectContextDiscovery::new()
        .discover(selected.path().to_owned())
        .await
        .unwrap();
    let mut model = selection();
    model.maximum_input_tokens = 20_000;
    let outcome = store
        .accept_session_input_with_context(
            PersistenceMutationRequestId::from_bytes([1; 16]),
            session,
            "u".repeat(6_000),
            model,
            crate::persistence::RunInputContext {
                skills: Default::default(),
                project: project.clone(),
                attachments: Vec::new(),
            },
        )
        .await;
    assert!(matches!(
        outcome,
        Err(crate::persistence::PersistenceError::ResourceLimit {
            resource: crate::persistence::PersistenceResourceLimit::Context
        })
    ));
    let accepted = store
        .accept_session_input_with_context(
            PersistenceMutationRequestId::from_bytes([2; 16]),
            session,
            "continue".to_owned(),
            selection(),
            crate::persistence::RunInputContext {
                skills: Default::default(),
                project: project.clone(),
                attachments: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert!(accepted.run.estimated_input_tokens as usize >= project.context_bytes());
    let loaded = store.load_run_context(accepted.run.id).await.unwrap();
    assert!(loaded.estimated_input_tokens as usize >= project.context_bytes());
    build_provider_request(&loaded, None).unwrap();
    store
        .finish_run_stopped(accepted.run.id, None)
        .await
        .unwrap();
    let other = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([3; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .unwrap();
    assert!(
        store
            .session_context_status(other.id, selection())
            .await
            .unwrap()
            .project_context
            .is_none()
    );
    let application = ServerApplication::from_session_store_for_test(store, "http://127.0.0.1:9");
    application
        .execute_for_local_owner(ApplicationRequest::SetSessionArchived {
            mutation_request_id: MutationRequestId::from_bytes([4; 16]),
            session_id: SessionId::from_bytes(*session.as_bytes()),
            archived: true,
        })
        .await
        .unwrap();
    application
        .execute_for_local_owner(ApplicationRequest::DeleteSession {
            mutation_request_id: MutationRequestId::from_bytes([5; 16]),
            session_id: SessionId::from_bytes(*session.as_bytes()),
        })
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(&source).unwrap().len(), 16 * 1024);
    let connection = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM run_project_contexts", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        0
    );
    application.shutdown().await;
    drop(application);
    SessionStore::open_for_test(root.path()).unwrap();
}
