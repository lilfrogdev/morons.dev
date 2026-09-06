use super::*;

#[tokio::test(flavor = "current_thread")]
async fn startup_completes_prepared_and_database_cleaned_session_deletions() {
    for (suffix, clean_database) in [("prepared", false), ("database-cleaned", true)] {
        let root = TestRoot::new(&format!("session-delete-recovery-{suffix}"));
        let selected = TestRoot::new(&format!("session-delete-recovery-selected-{suffix}"));
        let sentinel = selected.path().join("sentinel");
        fs::write(&sentinel, "keep").expect("sentinel should be written");
        let session_id = {
            let store = SessionStore::open_at(root.path()).expect("session store should open");
            let session = store
                .create_session_at(
                    MutationRequestId::from_bytes([0x5c; 16]),
                    None,
                    selected.path().to_string_lossy().into_owned(),
                )
                .await
                .expect("session should be created");
            store
                .set_session_archived(MutationRequestId::from_bytes([0x5d; 16]), session.id, true)
                .await
                .expect("session should archive");
            let request_id = MutationRequestId::from_bytes([0x5e; 16]);
            assert!(
                !store
                    .prepare_session_delete(request_id, session.id)
                    .await
                    .expect("deletion should prepare")
            );
            if clean_database {
                store
                    .clean_session_database(request_id)
                    .await
                    .expect("database cleanup should complete");
            }
            session.id
        };
        let reopened = SessionStore::open_at(root.path()).expect("deletion should recover");
        assert!(
            reopened
                .get_session(session_id)
                .await
                .expect("session query should succeed")
                .is_none()
        );
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "keep");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn startup_completes_a_prepared_session_archive_without_external_effects() {
    let root = TestRoot::new("prepared-session-archive");
    let selected = TestRoot::new("prepared-session-archive-selected");
    let sentinel = selected.path().join("sentinel");
    fs::write(&sentinel, "keep").expect("sentinel should be written");
    let session = {
        let store = SessionStore::open_at(root.path()).expect("session store should open");
        let session = store
            .create_session_at(
                MutationRequestId::from_bytes([0x55; 16]),
                None,
                selected.path().to_string_lossy().into_owned(),
            )
            .await
            .expect("session should be created");
        let (_, applied) = store
            .prepare_session_archive(MutationRequestId::from_bytes([0x56; 16]), session.id, true)
            .await
            .expect("archive should prepare");
        assert!(!applied);
        assert!(matches!(
            store
                .accept_local_command(
                    MutationRequestId::from_bytes([0x57; 16]),
                    session.id,
                    "printf blocked".to_owned(),
                    true,
                )
                .await,
            Err(PersistenceError::SessionArchived)
        ));
        session
    };
    let reopened = SessionStore::open_at(root.path()).expect("prepared archive should recover");
    assert!(
        reopened
            .get_session(session.id)
            .await
            .expect("session should load")
            .expect("session should exist")
            .archived
    );
    assert_eq!(fs::read_to_string(sentinel).unwrap(), "keep");
}

#[tokio::test(flavor = "current_thread")]
async fn startup_does_not_inspect_obsolete_private_workspace_contents() {
    let root = TestRoot::new("obsolete-workspace-ignored");
    let session = {
        let store = SessionStore::open_at(root.path()).expect("session store should open");
        store
            .create_session(MutationRequestId::from_bytes([0x34; 16]), None)
            .await
            .expect("session should be created")
    };
    fs::remove_dir_all(
        root.path()
            .join("workspaces")
            .join(encode_hex(&session.workspace_id)),
    )
    .expect("obsolete private workspace should be removable");

    let reopened = SessionStore::open_at(root.path())
        .expect("direct session startup must not inspect obsolete workspace contents");
    assert_eq!(
        reopened
            .get_session(session.id)
            .await
            .expect("session query should succeed"),
        Some(session)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn damaged_rebuildable_projections_are_restored_from_canonical_facts() {
    let root = TestRoot::new("projection-repair");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let session = store
        .create_session(MutationRequestId::from_bytes([0x55; 16]), None)
        .await
        .expect("session should be created");
    drop(store);

    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection =
        Connection::open(&database_path).expect("database should open for test damage");
    connection
        .execute("DELETE FROM sessions", [])
        .expect("session projection should be deleted");
    connection
        .execute("DELETE FROM delivery_events", [])
        .expect("event projection should be deleted");
    drop(connection);

    let reopened = SessionStore::open_at(root.path()).expect("projection repair should succeed");
    assert_eq!(
        reopened
            .get_session(session.id)
            .await
            .expect("repaired session should be queried"),
        Some(session)
    );
    drop(reopened);

    let connection = Connection::open(database_path).expect("repaired database should open");
    let delivery_events: i64 = connection
        .query_row("SELECT COUNT(*) FROM delivery_events", [], |row| row.get(0))
        .expect("event projection count should be readable");
    assert_eq!(delivery_events, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn startup_reconciles_a_dispatched_workspace_before_finalizing_the_session() {
    let root = TestRoot::new("workspace-recovery");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    drop(store);

    let request_id = [0x81; 16];
    let session_id = [0x82; 16];
    let workspace_id = [0x83; 16];
    let fingerprint = create_session_fingerprint(None);
    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection =
        Connection::open(&database_path).expect("database should open for crash setup");
    connection
        .execute(
            "INSERT INTO mutation_requests (
                request_id,
                operation_kind,
                accepted_sequence,
                accepted_at_milliseconds
            ) VALUES (?1, 1, 1, 1000)",
            [&request_id[..]],
        )
        .expect("mutation registry record should be inserted");
    connection
        .execute(
            "INSERT INTO session_creation_requests (
                request_id,
                operation_fingerprint,
                session_id,
                workspace_id,
                display_name,
                accepted_sequence,
                accepted_at_milliseconds,
                state
            ) VALUES (?1, ?2, ?3, ?4, NULL, 1, 1000, 1)",
            params![
                &request_id[..],
                &fingerprint[..],
                &session_id[..],
                &workspace_id[..]
            ],
        )
        .expect("prepared request should be inserted");
    connection
        .execute(
            "INSERT INTO audit_facts (
                audit_id,
                audit_sequence,
                request_id,
                session_id,
                audit_kind,
                created_at_milliseconds
            ) VALUES (?1, 2, ?2, ?3, 1, 1000)",
            params![&[0x85_u8; 16][..], &request_id[..], &session_id[..]],
        )
        .expect("accepted audit fact should be inserted");
    connection
        .execute(
            "INSERT INTO workspace_operation_facts (
                fact_id,
                fact_sequence,
                request_id,
                workspace_id,
                operation_kind,
                created_at_milliseconds
            ) VALUES (?1, 3, ?2, ?3, 1, 1001)",
            params![&[0x86_u8; 16][..], &request_id[..], &workspace_id[..]],
        )
        .expect("dispatch fact should be inserted");
    connection
        .execute(
            "INSERT INTO audit_facts (
                audit_id,
                audit_sequence,
                request_id,
                session_id,
                audit_kind,
                created_at_milliseconds
            ) VALUES (?1, 4, ?2, ?3, 2, 1001)",
            params![&[0x87_u8; 16][..], &request_id[..], &session_id[..]],
        )
        .expect("dispatch audit fact should be inserted");
    connection
        .execute(
            "UPDATE logical_sequences SET next_value = 5 WHERE singleton = 1",
            [],
        )
        .expect("logical sequence should advance past crash facts");
    drop(connection);

    let paths = StoragePaths::prepare(root.path()).expect("storage paths should remain valid");
    paths
        .provision_workspace(&workspace_id)
        .expect("workspace effect should complete before simulated crash");

    let recovered = SessionStore::open_at(root.path()).expect("startup recovery should succeed");
    let session = recovered
        .get_session(SessionId::from_bytes(session_id))
        .await
        .expect("recovered session should be queried")
        .expect("recovered session should exist");
    assert_eq!(session.workspace_id, workspace_id);
    drop(recovered);

    let connection = Connection::open(database_path).expect("recovered database should open");
    let state: i64 = connection
        .query_row(
            "SELECT state FROM session_creation_requests WHERE request_id = ?1",
            [&request_id[..]],
            |row| row.get(0),
        )
        .expect("request state should be readable");
    let workspace_facts: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM workspace_operation_facts WHERE request_id = ?1",
            [&request_id[..]],
            |row| row.get(0),
        )
        .expect("workspace fact count should be readable");
    assert_eq!(state, 2);
    assert_eq!(workspace_facts, 2);
}
