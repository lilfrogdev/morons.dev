use super::*;

#[test]
fn required_sqlite_configuration_is_applied_and_verified() {
    let root = TestRoot::new("sqlite-configuration");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let connection = database::open(&paths).expect("database should open");

    assert_eq!(pragma_integer(&connection, "PRAGMA page_size"), 4096);
    assert_eq!(pragma_integer(&connection, "PRAGMA synchronous"), 3);
    assert_eq!(pragma_integer(&connection, "PRAGMA fullfsync"), 1);
    assert_eq!(pragma_integer(&connection, "PRAGMA foreign_keys"), 1);
    assert_eq!(pragma_integer(&connection, "PRAGMA trusted_schema"), 0);
    assert_eq!(pragma_integer(&connection, "PRAGMA temp_store"), 2);
    assert!(
        connection
            .db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE)
            .expect("defensive mode should be queryable")
    );
    assert!(
        !connection
            .db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_WRITE)
            .expect("attach-write mode should be queryable")
    );
}

#[test]
fn newer_database_schema_fails_closed_without_downgrade() {
    let root = TestRoot::new("newer-schema");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    drop(store);

    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection =
        Connection::open(&database_path).expect("database should open for test change");
    connection
        .execute_batch("PRAGMA user_version = 27;")
        .expect("test schema version should change");
    drop(connection);

    let error = session_store_open_error(&root, "newer schema should fail closed");
    assert!(matches!(error, PersistenceError::InvalidState { .. }));

    let connection = Connection::open(database_path).expect("database should remain readable");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 27);
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_canonical_request_fingerprint_fails_closed() {
    let root = TestRoot::new("invalid-fingerprint");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let request_id = MutationRequestId::from_bytes([0x91; 16]);
    store
        .create_session(request_id, Some("Canonical input".to_owned()))
        .await
        .expect("session should be created");
    drop(store);

    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection = Connection::open(&database_path).expect("database should open for corruption");
    connection
        .execute(
            "UPDATE session_creation_requests SET operation_fingerprint = ?2 WHERE request_id = ?1",
            params![&request_id.as_bytes()[..], &[0_u8; 32][..]],
        )
        .expect("test fingerprint should be corrupted");
    drop(connection);

    let error = session_store_open_error(&root, "invalid fingerprint should fail closed");
    assert!(matches!(error, PersistenceError::InvalidState { .. }));
}

#[cfg(unix)]
#[test]
fn insecure_existing_data_directory_fails_closed() {
    let root = TestRoot::new("insecure-data-directory");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    drop(store);

    let data_directory = root.path().join("data");
    fs::set_permissions(&data_directory, fs::Permissions::from_mode(0o755))
        .expect("test should weaken data permissions");
    let error = session_store_open_error(&root, "insecure data root should fail closed");
    assert!(matches!(error, PersistenceError::InvalidState { .. }));
}

#[cfg(unix)]
#[test]
fn linked_database_journal_fails_closed() {
    let root = TestRoot::new("linked-journal");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    drop(store);

    let journal_path = root.path().join("data").join("sessions.sqlite3-journal");
    std::os::unix::fs::symlink(root.path().join("missing-journal-target"), journal_path)
        .expect("test journal symlink should be created");

    let error = session_store_open_error(&root, "linked journal should fail closed");
    assert!(matches!(error, PersistenceError::InvalidState { .. }));
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn database_and_workspace_state_are_owner_only() {
    let root = TestRoot::new("owner-only-state");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let session = store
        .create_session(MutationRequestId::from_bytes([0x66; 16]), None)
        .await
        .expect("session should be created");
    store
        .set_open_code_credential(
            MutationRequestId::from_bytes([0x67; 16]),
            0,
            b"not-a-real-permission-key".to_vec(),
        )
        .await
        .expect("credential should be configured");

    assert_mode(&root.path().join("data"), 0o700);
    assert_mode(&root.path().join("workspaces"), 0o700);
    assert_mode(&root.path().join("sandbox-operations"), 0o700);
    assert_mode(&root.path().join("backups"), 0o700);
    assert_mode(&root.path().join("attachments"), 0o700);
    assert_mode(&root.path().join("credentials"), 0o700);
    assert_mode(&root.path().join("data").join("sessions.sqlite3"), 0o600);
    assert_mode(
        &root.path().join("credentials").join("opencode.state"),
        0o600,
    );
    let workspace = root
        .path()
        .join("workspaces")
        .join(encode_hex(&session.workspace_id));
    assert_mode(&workspace, 0o700);
    assert_mode(&workspace.join("identity"), 0o600);
    assert!(
        !root
            .path()
            .join("data")
            .join("sessions.sqlite3-journal")
            .exists()
    );
}
