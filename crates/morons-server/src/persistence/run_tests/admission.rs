use super::*;

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
async fn unavailable_working_directory_rejects_run_before_transcript_commit() {
    let root = TestRoot::new("unavailable-run-directory");
    let selected = TestRoot::new("selected-run-directory");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    configure_credential(&store).await;
    let session = store
        .create_session_at(
            MutationRequestId::from_bytes([0x09; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    fs::remove_dir_all(selected.path()).expect("selected directory should be removed");

    assert!(matches!(
        store
            .accept_session_input(
                MutationRequestId::from_bytes([0x0a; 16]),
                session.id,
                "must not commit".to_owned(),
                model_selection(),
            )
            .await,
        Err(PersistenceError::WorkingDirectoryUnavailable)
    ));
    let page = store
        .list_session_transcript(session.id, None, 1)
        .await
        .expect("transcript should remain readable");
    assert!(page.entries.is_empty());
    assert!(page.runs.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn archived_sessions_reject_new_runs_before_transcript_commit() {
    let root = TestRoot::new("archived-run-input");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    configure_credential(&store).await;
    let session = store
        .create_session(MutationRequestId::from_bytes([0x0b; 16]), None)
        .await
        .expect("session should be created");
    store
        .set_session_archived(MutationRequestId::from_bytes([0x0c; 16]), session.id, true)
        .await
        .expect("session should archive");
    assert!(matches!(
        store
            .accept_session_input(
                MutationRequestId::from_bytes([0x0d; 16]),
                session.id,
                "must not commit".to_owned(),
                model_selection(),
            )
            .await,
        Err(PersistenceError::SessionArchived)
    ));
    let page = store
        .list_session_transcript(session.id, None, 1)
        .await
        .expect("transcript should remain readable");
    assert!(page.entries.is_empty());
    assert!(page.runs.is_empty());
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
    assert_eq!(
        accepted.run.tool_catalog_version,
        crate::tools::TOOL_CATALOG_VERSION
    );
    assert_eq!(
        accepted.run.tool_limits_version,
        crate::tools::TOOL_LIMITS_VERSION
    );
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
