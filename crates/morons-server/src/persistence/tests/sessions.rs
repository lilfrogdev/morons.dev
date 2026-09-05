use super::*;

#[tokio::test(flavor = "current_thread")]
async fn sessions_are_idempotent_queryable_paginated_and_durable() {
    let root = TestRoot::new("session-roundtrip");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let first = store
        .create_session(
            MutationRequestId::from_bytes([0x11; 16]),
            Some("First session".to_owned()),
        )
        .await
        .expect("first session should be created");
    let expected_working_directory = std::env::current_dir()
        .expect("test working directory should resolve")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        first.working_directory.as_deref(),
        Some(expected_working_directory.as_str())
    );
    let retry = store
        .create_session(
            MutationRequestId::from_bytes([0x11; 16]),
            Some("First session".to_owned()),
        )
        .await
        .expect("an exact retry should return the original session");
    assert_eq!(retry, first);
    assert!(matches!(
        store
            .create_session_at(
                MutationRequestId::from_bytes([0x11; 16]),
                Some("First session".to_owned()),
                root.path().to_string_lossy().into_owned(),
            )
            .await,
        Err(PersistenceError::RequestConflict)
    ));
    assert!(matches!(
        store
            .create_session_at(
                MutationRequestId::from_bytes([0x12; 16]),
                None,
                root.path()
                    .join("missing-working-directory")
                    .to_string_lossy()
                    .into_owned(),
            )
            .await,
        Err(PersistenceError::InvalidInput { .. })
    ));

    let second = store
        .create_session(
            MutationRequestId::from_bytes([0x22; 16]),
            Some("Second session".to_owned()),
        )
        .await
        .expect("second session should be created");
    let third = store
        .create_session(MutationRequestId::from_bytes([0x33; 16]), None)
        .await
        .expect("third session should be created");

    assert_eq!(
        store
            .get_session(first.id)
            .await
            .expect("session lookup should succeed"),
        Some(first.clone())
    );
    assert_eq!(
        store
            .get_session(SessionId::from_bytes([0xff; 16]))
            .await
            .expect("missing session lookup should succeed"),
        None
    );

    let first_page = store
        .list_sessions(None, 2)
        .await
        .expect("first page should be listed");
    assert_eq!(first_page.sessions, vec![first.clone(), second.clone()]);
    let cursor = first_page
        .next_cursor
        .expect("first page should have a continuation cursor");
    let snapshot_catalog_cursor = first_page.catalog_cursor;
    let fourth = store
        .create_session(
            MutationRequestId::from_bytes([0x34; 16]),
            Some("Fourth session".to_owned()),
        )
        .await
        .expect("fourth session should be created");
    let second_page = store
        .list_sessions(Some(cursor), 2)
        .await
        .expect("second page should be listed");
    assert_eq!(second_page.sessions, vec![third.clone()]);
    assert_eq!(second_page.next_cursor, None);
    assert_eq!(second_page.catalog_cursor, snapshot_catalog_cursor);

    let replay = store
        .read_session_catalog_events(snapshot_catalog_cursor, 100)
        .await
        .expect("events after the snapshot should replay");
    assert_eq!(replay.events.len(), 1);
    assert_eq!(replay.events[0].session(), Some(&fourth));
    assert_eq!(replay.events[0].cursor, replay.high_water);

    let complete_replay = store
        .read_session_catalog_events(SessionCatalogEventCursor::from_sequence(0), 100)
        .await
        .expect("complete event history should replay");
    assert_eq!(complete_replay.events.len(), 4);
    assert_eq!(
        complete_replay
            .events
            .iter()
            .map(|event| event
                .session()
                .expect("creation event should contain a session")
                .id)
            .collect::<Vec<_>>(),
        vec![first.id, second.id, third.id, fourth.id]
    );

    let first_workspace_id = first.workspace_id;
    drop(store);

    let reopened = SessionStore::open_at(root.path()).expect("session store should reopen");
    let persisted = reopened
        .get_session(first.id)
        .await
        .expect("persisted session should be readable")
        .expect("persisted session should exist");
    assert_eq!(persisted, first);
    assert_eq!(persisted.workspace_id, first_workspace_id);
    let replay_after_restart = reopened
        .read_session_catalog_events(snapshot_catalog_cursor, 100)
        .await
        .expect("durable events should replay after restart");
    assert_eq!(replay_after_restart.events.len(), 1);
    assert_eq!(replay_after_restart.events[0].session(), Some(&fourth));
}

#[tokio::test(flavor = "current_thread")]
async fn session_renames_are_idempotent_durable_and_snapshot_consistent() {
    let root = TestRoot::new("session-rename");
    let selected = TestRoot::new("session-rename-selected");
    let sentinel = selected.path().join("must-remain.txt");
    fs::write(&sentinel, "unchanged").expect("sentinel should be written");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    store
        .create_session_at(
            MutationRequestId::from_bytes([0x40; 16]),
            Some("Leading".to_owned()),
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("leading session should be created");
    let created = store
        .create_session_at(
            MutationRequestId::from_bytes([0x41; 16]),
            Some("Before".to_owned()),
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let snapshot = store
        .list_sessions(None, 1)
        .await
        .expect("pre-rename snapshot should load");
    let renamed = store
        .rename_session(
            MutationRequestId::from_bytes([0x42; 16]),
            created.id,
            "After".to_owned(),
        )
        .await
        .expect("session should be renamed");
    assert_eq!(renamed.display_name.as_deref(), Some("After"));
    assert_eq!(renamed.working_directory, created.working_directory);
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "unchanged");
    assert_eq!(
        store
            .rename_session(
                MutationRequestId::from_bytes([0x42; 16]),
                created.id,
                "After".to_owned(),
            )
            .await
            .expect("exact rename should retry"),
        renamed
    );
    assert!(matches!(
        store
            .rename_session(
                MutationRequestId::from_bytes([0x42; 16]),
                created.id,
                "Conflict".to_owned(),
            )
            .await,
        Err(PersistenceError::RequestConflict)
    ));
    let old_snapshot = store
        .list_sessions(snapshot.next_cursor, 1)
        .await
        .expect("old snapshot should remain readable");
    assert_eq!(old_snapshot.sessions.len(), 1);
    assert_eq!(old_snapshot.sessions[0].id, created.id);
    assert_eq!(
        old_snapshot.sessions[0].display_name.as_deref(),
        Some("Before")
    );
    let replay = store
        .read_session_catalog_events(snapshot.catalog_cursor, 10)
        .await
        .expect("rename event should replay");
    assert_eq!(replay.events.len(), 1);
    assert_eq!(replay.events[0].session(), Some(&renamed));
    drop(store);

    let reopened = SessionStore::open_at(root.path()).expect("renamed session should reopen");
    assert_eq!(
        reopened
            .get_session(created.id)
            .await
            .expect("renamed session should load")
            .expect("renamed session should exist")
            .display_name
            .as_deref(),
        Some("After")
    );
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "unchanged");
}

#[tokio::test(flavor = "current_thread")]
async fn session_archiving_is_idempotent_durable_and_snapshot_consistent() {
    let root = TestRoot::new("session-archive");
    let selected = TestRoot::new("session-archive-selected");
    let sentinel = selected.path().join("must-remain.txt");
    fs::write(&sentinel, "unchanged").expect("sentinel should be written");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    store
        .create_session_at(
            MutationRequestId::from_bytes([0x50; 16]),
            Some("Leading".to_owned()),
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("leading session should be created");
    let created = store
        .create_session_at(
            MutationRequestId::from_bytes([0x51; 16]),
            Some("Subject".to_owned()),
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let snapshot = store
        .list_sessions(None, 1)
        .await
        .expect("pre-archive snapshot should load");
    let archived = store
        .set_session_archived(MutationRequestId::from_bytes([0x52; 16]), created.id, true)
        .await
        .expect("session should archive");
    assert!(archived.archived);
    assert_eq!(archived.working_directory, created.working_directory);
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "unchanged");
    assert_eq!(
        store
            .set_session_archived(MutationRequestId::from_bytes([0x52; 16]), created.id, true,)
            .await
            .expect("exact archive should retry"),
        archived
    );
    assert!(matches!(
        store
            .set_session_archived(MutationRequestId::from_bytes([0x52; 16]), created.id, false,)
            .await,
        Err(PersistenceError::RequestConflict)
    ));
    assert!(matches!(
        store
            .accept_local_command(
                MutationRequestId::from_bytes([0x53; 16]),
                created.id,
                "printf blocked".to_owned(),
                true,
            )
            .await,
        Err(PersistenceError::SessionArchived)
    ));
    let old_snapshot = store
        .list_sessions(snapshot.next_cursor, 1)
        .await
        .expect("old snapshot should remain readable");
    assert_eq!(old_snapshot.sessions.len(), 1);
    assert!(!old_snapshot.sessions[0].archived);
    let replay = store
        .read_session_catalog_events(snapshot.catalog_cursor, 10)
        .await
        .expect("archive event should replay");
    assert_eq!(replay.events.len(), 1);
    assert!(
        replay.events[0]
            .session()
            .expect("archive event should contain a session")
            .archived
    );
    let restored = store
        .set_session_archived(MutationRequestId::from_bytes([0x54; 16]), created.id, false)
        .await
        .expect("session should unarchive");
    assert!(!restored.archived);
    drop(store);

    let reopened = SessionStore::open_at(root.path()).expect("session store should reopen");
    assert!(
        !reopened
            .get_session(created.id)
            .await
            .expect("session should load")
            .expect("session should exist")
            .archived
    );
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "unchanged");
}

#[tokio::test(flavor = "current_thread")]
async fn session_deletion_is_idempotent_tombstoned_and_never_touches_the_selected_directory() {
    let root = TestRoot::new("session-delete");
    let selected = TestRoot::new("session-delete-selected");
    let sentinel = selected.path().join("sentinel");
    fs::write(&sentinel, "keep").expect("sentinel should be written");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let creation_request = MutationRequestId::from_bytes([0x58; 16]);
    let session = store
        .create_session_at(
            creation_request,
            Some("Delete me".to_owned()),
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let retained = store
        .create_session_at(
            MutationRequestId::from_bytes([0x59; 16]),
            Some("Retained".to_owned()),
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("retained session should be created");
    let delete_request = MutationRequestId::from_bytes([0x5b; 16]);
    assert!(matches!(
        store.delete_session(delete_request, session.id).await,
        Err(PersistenceError::SessionNotArchived)
    ));
    store
        .set_session_archived(MutationRequestId::from_bytes([0x5a; 16]), session.id, true)
        .await
        .expect("session should archive");
    let before_delete = store
        .list_sessions(None, 100)
        .await
        .expect("catalog snapshot should load");
    assert_eq!(
        store
            .delete_session(delete_request, session.id)
            .await
            .expect("session should delete"),
        session.id
    );
    assert!(
        store
            .get_session(session.id)
            .await
            .expect("deleted session query should succeed")
            .is_none()
    );
    let sessions = store
        .list_sessions(None, 100)
        .await
        .expect("catalog should remain readable")
        .sessions;
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, retained.id);
    let replay = store
        .read_session_catalog_events(before_delete.catalog_cursor, 10)
        .await
        .expect("deletion should replay");
    assert_eq!(replay.events.len(), 1);
    assert!(matches!(
        replay.events[0].kind,
        SessionCatalogEventKind::Removed(removed) if removed == session.id
    ));
    assert_eq!(
        store
            .delete_session(delete_request, session.id)
            .await
            .expect("exact deletion should retry"),
        session.id
    );
    assert!(matches!(
        store.delete_session(delete_request, retained.id).await,
        Err(PersistenceError::RequestConflict)
    ));
    assert!(matches!(
        store
            .create_session_at(
                creation_request,
                Some("Reused".to_owned()),
                selected.path().to_string_lossy().into_owned(),
            )
            .await,
        Err(PersistenceError::RequestConflict)
    ));
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "keep");
    drop(store);

    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection = Connection::open(&database_path).expect("database should remain readable");
    for table in [
        "sessions",
        "session_run_states",
        "session_created_facts",
        "session_creation_requests",
        "session_rename_requests",
        "session_archive_requests",
        "session_entries",
        "run_input_requests",
        "run_accepted_facts",
        "run_state_facts",
        "local_commands",
        "context_checkpoints",
        "image_attachments",
        "tool_image_attachments",
    ] {
        let count: i64 = connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE session_id = ?1"),
                [&session.id.as_bytes()[..]],
                |row| row.get(0),
            )
            .expect("session-scoped cleanup should remain queryable");
        assert_eq!(
            count, 0,
            "{table} should contain no deleted session records"
        );
    }
    let tombstones: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM deleted_mutation_tombstones
             WHERE delete_request_id = ?1",
            [&delete_request.as_bytes()[..]],
            |row| row.get(0),
        )
        .expect("idempotency tombstones should remain queryable");
    assert!(tombstones >= 2);
    connection
        .execute("DELETE FROM delivery_events WHERE event_kind = 20", [])
        .expect("rebuildable deletion event should be removable for repair test");
    drop(connection);

    let reopened = SessionStore::open_at(root.path()).expect("deleted state should reopen");
    assert!(
        reopened
            .get_session(session.id)
            .await
            .expect("deleted session query should succeed")
            .is_none()
    );
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "keep");
    let repaired = reopened
        .read_session_catalog_events(SessionCatalogEventCursor::from_sequence(0), 100)
        .await
        .expect("deletion event projection should rebuild");
    assert!(repaired.events.iter().any(|event| matches!(
        &event.kind,
        SessionCatalogEventKind::Removed(removed) if *removed == session.id
    )));
}

#[tokio::test(flavor = "current_thread")]
async fn session_deletion_fails_before_mutation_when_selected_and_attachment_directories_overlap() {
    let root = TestRoot::new("session-delete-overlap");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let selected = root.path().join("attachments");
    let sentinel = selected.join("sentinel");
    fs::write(&sentinel, "keep").expect("selected-directory sentinel should be written");
    let session = store
        .create_session_at(
            MutationRequestId::from_bytes([0x66; 16]),
            None,
            selected.to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be available");
    let (attachment_path, mut attachment) = paths
        .create_attachment_file(session.id.as_bytes(), &[0x67; 16])
        .expect("private attachment fixture should be created");
    attachment
        .write_all(b"orphan")
        .expect("attachment fixture should be written");
    attachment.sync_all().expect("attachment should sync");
    drop(attachment);
    store
        .set_session_archived(MutationRequestId::from_bytes([0x68; 16]), session.id, true)
        .await
        .expect("session should archive");
    assert!(matches!(
        store
            .delete_session(MutationRequestId::from_bytes([0x69; 16]), session.id)
            .await,
        Err(PersistenceError::InvalidInput { .. })
    ));
    assert_eq!(fs::read_to_string(sentinel).unwrap(), "keep");
    assert!(attachment_path.exists());
    assert!(
        store
            .get_session(session.id)
            .await
            .expect("session query should succeed")
            .is_some()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn exact_session_retry_survives_an_unavailable_working_directory() {
    let root = TestRoot::new("session-directory-retry");
    let selected = TestRoot::new("selected-directory-retry");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let request_id = MutationRequestId::from_bytes([0x35; 16]);
    let working_directory = selected.path().to_string_lossy().into_owned();
    let created = store
        .create_session_at(request_id, None, working_directory.clone())
        .await
        .expect("session should be created");
    fs::remove_dir_all(selected.path()).expect("selected directory should be removed");

    let retried = store
        .create_session_at(request_id, None, working_directory)
        .await
        .expect("exact retry should return durable session state");
    assert_eq!(retried, created);
}

#[tokio::test(flavor = "current_thread")]
async fn conflicting_request_identifiers_fail_without_creating_another_session() {
    let root = TestRoot::new("request-conflict");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let request_id = MutationRequestId::from_bytes([0x44; 16]);
    store
        .create_session(request_id, Some("Original".to_owned()))
        .await
        .expect("original request should succeed");

    let error = store
        .create_session(request_id, Some("Changed".to_owned()))
        .await
        .expect_err("conflicting retry should fail");
    assert!(matches!(error, PersistenceError::RequestConflict));
    assert_eq!(
        store
            .list_sessions(None, 100)
            .await
            .expect("sessions should be listed")
            .sessions
            .len(),
        1
    );
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_requests_fail_before_reaching_the_worker() {
    let root = TestRoot::new("invalid-input");
    let store = SessionStore::open_at(root.path()).expect("session store should open");

    let zero_identifier = store
        .create_session(MutationRequestId::from_bytes([0; 16]), None)
        .await
        .expect_err("zero request identifier should fail");
    assert!(matches!(
        zero_identifier,
        PersistenceError::InvalidInput { .. }
    ));

    let zero_credential_identifier = store
        .set_open_code_credential(
            MutationRequestId::from_bytes([0; 16]),
            0,
            b"not-a-real-key".to_vec(),
        )
        .await
        .expect_err("zero credential request identifier should fail");
    assert!(matches!(
        zero_credential_identifier,
        PersistenceError::InvalidInput { .. }
    ));

    let invalid_api_key = store
        .set_open_code_credential(
            MutationRequestId::from_bytes([3; 16]),
            0,
            b"invalid key".to_vec(),
        )
        .await
        .expect_err("invalid API key should fail");
    assert!(matches!(
        invalid_api_key,
        PersistenceError::InvalidInput { .. }
    ));

    let control_character = store
        .create_session(
            MutationRequestId::from_bytes([1; 16]),
            Some("invalid\nname".to_owned()),
        )
        .await
        .expect_err("control character should fail");
    assert!(matches!(
        control_character,
        PersistenceError::InvalidInput { .. }
    ));

    let oversized = store
        .create_session(
            MutationRequestId::from_bytes([2; 16]),
            Some("x".repeat(257)),
        )
        .await
        .expect_err("oversized name should fail");
    assert!(matches!(oversized, PersistenceError::InvalidInput { .. }));

    let empty_page = store
        .list_sessions(None, 0)
        .await
        .expect_err("zero page size should fail");
    assert!(matches!(empty_page, PersistenceError::InvalidInput { .. }));

    let oversized_cursor = store
        .list_sessions(Some(SessionListCursor::new(u64::MAX, 0)), 1)
        .await
        .expect_err("oversized cursor should fail");
    assert!(matches!(
        oversized_cursor,
        PersistenceError::InvalidInput { .. }
    ));

    let empty_event_page = store
        .read_session_catalog_events(SessionCatalogEventCursor::from_sequence(0), 0)
        .await
        .expect_err("zero event page size should fail");
    assert!(matches!(
        empty_event_page,
        PersistenceError::InvalidInput { .. }
    ));

    let future_catalog_cursor = store
        .read_session_catalog_events(SessionCatalogEventCursor::from_sequence(1), 1)
        .await
        .expect_err("future event cursor should fail");
    assert!(matches!(
        future_catalog_cursor,
        PersistenceError::InvalidInput { .. }
    ));
}
