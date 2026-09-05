use super::*;

#[test]
fn schema_version_one_migrates_to_current_version() {
    let root = TestRoot::new("schema-v1-migration");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let (initialization_path, file) = paths
        .create_database_initialization_file(&[0xe1; 16])
        .expect("initialization file should be created");
    drop(file);
    let connection =
        Connection::open(&initialization_path).expect("version one migration fixture should open");
    connection
        .execute_batch(include_str!("../schema_v1.sql"))
        .expect("version one schema should initialize");
    let request_id = [0xe5_u8; 16];
    let session_id = [0xe6_u8; 16];
    let workspace_id = [0xe7_u8; 16];
    let fingerprint = create_session_fingerprint(None);
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
             ) VALUES (?1, ?2, ?3, ?4, NULL, 1, 1000, 0)",
            params![
                &request_id[..],
                &fingerprint[..],
                &session_id[..],
                &workspace_id[..]
            ],
        )
        .expect("version one request fixture should be inserted");
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
            params![&[0xe8_u8; 16][..], &request_id[..], &session_id[..]],
        )
        .expect("version one audit fixture should be inserted");
    connection
        .execute(
            "UPDATE logical_sequences SET next_value = 3 WHERE singleton = 1",
            [],
        )
        .expect("version one sequence should advance past fixtures");
    drop(connection);
    paths
        .install_database(&initialization_path)
        .expect("version one database should install");

    let connection = database::open(&paths).expect("version one database should migrate");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 25);
    let mutation_operation: i64 = connection
        .query_row(
            "SELECT operation_kind FROM mutation_requests WHERE request_id = ?1",
            [&request_id[..]],
            |row| row.get(0),
        )
        .expect("migrated request should be registered");
    assert_eq!(mutation_operation, 1);
}

#[test]
fn stale_private_migration_backup_temporary_file_is_removed() {
    let root = TestRoot::new("stale-migration-backup");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let (temporary_path, file) = paths
        .create_migration_backup_file(2, &[0xc1; 16])
        .expect("temporary migration backup should be created");
    drop(file);
    drop(paths);

    StoragePaths::prepare(root.path()).expect("stale backup cleanup should succeed");
    assert!(!temporary_path.exists());
}

#[test]
fn schema_version_two_migrates_to_current_version() {
    let root = TestRoot::new("schema-v2-migration");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let (initialization_path, file) = paths
        .create_database_initialization_file(&[0xd1; 16])
        .expect("initialization file should be created");
    drop(file);
    let connection =
        Connection::open(&initialization_path).expect("version two fixture should open");
    connection
        .execute_batch(include_str!("../schema_v1.sql"))
        .expect("version one schema should initialize");
    connection
        .execute_batch(include_str!("../schema_v2.sql"))
        .expect("version two schema should initialize");
    let request_id = [0xd2_u8; 16];
    connection
        .execute(
            "INSERT INTO mutation_requests (
                request_id, operation_kind, accepted_sequence, accepted_at_milliseconds
             ) VALUES (?1, 3, 1, 1000)",
            [&request_id[..]],
        )
        .expect("version two mutation should be inserted");
    connection
        .execute(
            "INSERT INTO credential_mutation_requests (
                request_id, operation_kind, expected_generation,
                accepted_sequence, accepted_at_milliseconds, state,
                result_generation, result_configured
             ) VALUES (?1, 3, 0, 1, 1000, 3, NULL, NULL)",
            [&request_id[..]],
        )
        .expect("version two credential request should be inserted");
    connection
        .execute(
            "UPDATE logical_sequences SET next_value = 2 WHERE singleton = 1",
            [],
        )
        .expect("version two sequence should advance past fixture");
    drop(connection);
    paths
        .install_database(&initialization_path)
        .expect("version two database should install");

    let connection = database::open(&paths).expect("version two database should migrate");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 25);
    let operation: i64 = connection
        .query_row(
            "SELECT operation_kind FROM mutation_requests WHERE request_id = ?1",
            [&request_id[..]],
            |row| row.get(0),
        )
        .expect("version two mutation should survive migration");
    assert_eq!(operation, 3);
    let backup_path = root
        .path()
        .join("backups")
        .join("sessions-before-schema-v2.sqlite3");
    let backup = Connection::open(&backup_path).expect("migration backup should open");
    assert_eq!(pragma_integer(&backup, "PRAGMA user_version"), 2);
    let backup_operation: i64 = backup
        .query_row(
            "SELECT operation_kind FROM mutation_requests WHERE request_id = ?1",
            [&request_id[..]],
            |row| row.get(0),
        )
        .expect("migration backup should preserve version two state");
    assert_eq!(backup_operation, 3);
    #[cfg(unix)]
    assert_mode(&backup_path, 0o600);
}

#[test]
fn schema_version_three_migrates_to_current_version() {
    let root = TestRoot::new("schema-v3-migration");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let (initialization_path, file) = paths
        .create_database_initialization_file(&[0xc3; 16])
        .expect("initialization file should be created");
    drop(file);
    let connection =
        Connection::open(&initialization_path).expect("version three fixture should open");
    connection
        .execute_batch(include_str!("../schema_v1.sql"))
        .expect("version one schema should initialize");
    connection
        .execute_batch(include_str!("../schema_v2.sql"))
        .expect("version two schema should initialize");
    connection
        .execute_batch(include_str!("../schema_v3.sql"))
        .expect("version three schema should initialize");
    drop(connection);
    paths
        .install_database(&initialization_path)
        .expect("version three database should install");

    let connection = database::open(&paths).expect("version three database should migrate");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 25);
    let stop_table: String = connection
        .query_row(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND name = 'server_stop_requests'",
            [],
            |row| row.get(0),
        )
        .expect("server stop table should exist");
    assert_eq!(stop_table, "server_stop_requests");
    let backup_path = root
        .path()
        .join("backups")
        .join("sessions-before-schema-v3.sqlite3");
    let backup = Connection::open(backup_path).expect("version three backup should open");
    assert_eq!(pragma_integer(&backup, "PRAGMA user_version"), 3);
}

#[test]
fn schema_version_four_migrates_to_current_version() {
    let root = TestRoot::new("schema-v4-migration");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let (initialization_path, file) = paths
        .create_database_initialization_file(&[0xc4; 16])
        .expect("initialization file should be created");
    drop(file);
    let connection =
        Connection::open(&initialization_path).expect("version four fixture should open");
    connection
        .execute_batch(include_str!("../schema_v1.sql"))
        .expect("version one schema should initialize");
    connection
        .execute_batch(include_str!("../schema_v2.sql"))
        .expect("version two schema should initialize");
    connection
        .execute_batch(include_str!("../schema_v3.sql"))
        .expect("version three schema should initialize");
    connection
        .execute_batch(include_str!("../schema_v4.sql"))
        .expect("version four schema should initialize");
    drop(connection);
    paths
        .install_database(&initialization_path)
        .expect("version four database should install");

    let connection = database::open(&paths).expect("version four database should migrate");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 25);
    let import_table: String = connection
        .query_row(
            "SELECT name FROM sqlite_schema
             WHERE type = 'table' AND name = 'repository_import_requests'",
            [],
            |row| row.get(0),
        )
        .expect("repository import table should exist");
    assert_eq!(import_table, "repository_import_requests");
    let backup_path = root
        .path()
        .join("backups")
        .join("sessions-before-schema-v4.sqlite3");
    let backup = Connection::open(backup_path).expect("version four backup should open");
    assert_eq!(pragma_integer(&backup, "PRAGMA user_version"), 4);
}

#[test]
fn schema_version_five_migrates_to_current_version() {
    let root = TestRoot::new("schema-v5-migration");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let (initialization_path, file) = paths
        .create_database_initialization_file(&[0xc5; 16])
        .expect("initialization file should be created");
    drop(file);
    let connection =
        Connection::open(&initialization_path).expect("version five fixture should open");
    connection
        .execute_batch(include_str!("../schema_v1.sql"))
        .expect("version one schema should initialize");
    connection
        .execute_batch(include_str!("../schema_v2.sql"))
        .expect("version two schema should initialize");
    connection
        .execute_batch(include_str!("../schema_v3.sql"))
        .expect("version three schema should initialize");
    connection
        .execute_batch(include_str!("../schema_v4.sql"))
        .expect("version four schema should initialize");
    connection
        .execute_batch(include_str!("../schema_v5.sql"))
        .expect("version five schema should initialize");
    drop(connection);
    paths
        .install_database(&initialization_path)
        .expect("version five database should install");

    let connection = database::open(&paths).expect("version five database should migrate");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 25);
    let tool_table: String = connection
        .query_row(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND name = 'tool_calls'",
            [],
            |row| row.get(0),
        )
        .expect("tool call table should exist");
    assert_eq!(tool_table, "tool_calls");
    let backup_path = root
        .path()
        .join("backups")
        .join("sessions-before-schema-v5.sqlite3");
    let backup = Connection::open(backup_path).expect("version five backup should open");
    assert_eq!(pragma_integer(&backup, "PRAGMA user_version"), 5);
}

#[test]
fn schema_version_six_migrates_to_current_version() {
    let root = TestRoot::new("schema-v6-migration");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let (initialization_path, file) = paths
        .create_database_initialization_file(&[0xc6; 16])
        .expect("version six initialization file should be created");
    drop(file);
    let connection =
        Connection::open(&initialization_path).expect("version six fixture should open");
    for schema in [
        include_str!("../schema_v1.sql"),
        include_str!("../schema_v2.sql"),
        include_str!("../schema_v3.sql"),
        include_str!("../schema_v4.sql"),
        include_str!("../schema_v5.sql"),
        include_str!("../schema_v6.sql"),
    ] {
        connection
            .execute_batch(schema)
            .expect("schema fixture should migrate");
    }
    drop(connection);
    paths
        .install_database(&initialization_path)
        .expect("version six database should install");

    let connection = database::open(&paths).expect("version six database should migrate");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 25);
    let image_table: String = connection
        .query_row(
            "SELECT name FROM sqlite_schema
             WHERE type = 'table' AND name = 'execution_image_requests'",
            [],
            |row| row.get(0),
        )
        .expect("execution image table should exist");
    assert_eq!(image_table, "execution_image_requests");
    let backup = Connection::open(
        root.path()
            .join("backups")
            .join("sessions-before-schema-v6.sqlite3"),
    )
    .expect("version six backup should open");
    assert_eq!(pragma_integer(&backup, "PRAGMA user_version"), 6);
}

#[test]
fn schema_version_seven_migrates_to_current_version() {
    let root = TestRoot::new("schema-v7-migration");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let (initialization_path, file) = paths
        .create_database_initialization_file(&[0xc7; 16])
        .expect("version seven initialization file should be created");
    drop(file);
    let connection =
        Connection::open(&initialization_path).expect("version seven fixture should open");
    for schema in [
        include_str!("../schema_v1.sql"),
        include_str!("../schema_v2.sql"),
        include_str!("../schema_v3.sql"),
        include_str!("../schema_v4.sql"),
        include_str!("../schema_v5.sql"),
        include_str!("../schema_v6.sql"),
        include_str!("../schema_v7.sql"),
    ] {
        connection
            .execute_batch(schema)
            .expect("schema fixture should migrate");
    }
    drop(connection);
    paths
        .install_database(&initialization_path)
        .expect("version seven database should install");
    let connection = database::open(&paths).expect("version seven database should migrate");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 25);
    let generation_table: String = connection
        .query_row(
            "SELECT name FROM sqlite_schema
             WHERE type = 'table' AND name = 'worktree_generation_facts'",
            [],
            |row| row.get(0),
        )
        .expect("generation table should exist");
    assert_eq!(generation_table, "worktree_generation_facts");
    let backup = Connection::open(
        root.path()
            .join("backups")
            .join("sessions-before-schema-v7.sqlite3"),
    )
    .expect("version seven backup should open");
    assert_eq!(pragma_integer(&backup, "PRAGMA user_version"), 7);
}

#[test]
fn schema_version_eight_migrates_to_current_version() {
    let root = TestRoot::new("schema-v8-migration");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let (initialization_path, file) = paths
        .create_database_initialization_file(&[0xc8; 16])
        .expect("version eight initialization file should be created");
    drop(file);
    let connection =
        Connection::open(&initialization_path).expect("version eight fixture should open");
    for schema in [
        include_str!("../schema_v1.sql"),
        include_str!("../schema_v2.sql"),
        include_str!("../schema_v3.sql"),
        include_str!("../schema_v4.sql"),
        include_str!("../schema_v5.sql"),
        include_str!("../schema_v6.sql"),
        include_str!("../schema_v7.sql"),
        include_str!("../schema_v8.sql"),
    ] {
        connection
            .execute_batch(schema)
            .expect("schema fixture should migrate");
    }
    drop(connection);
    paths
        .install_database(&initialization_path)
        .expect("version eight database should install");
    let connection = database::open(&paths).expect("version eight database should migrate");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 25);
    let column: String = connection
        .query_row(
            "SELECT name FROM pragma_table_info('run_accepted_facts')
             WHERE name = 'execution_image_generation'",
            [],
            |row| row.get(0),
        )
        .expect("image generation column should exist");
    assert_eq!(column, "execution_image_generation");
    let backup = Connection::open(
        root.path()
            .join("backups/sessions-before-schema-v8.sqlite3"),
    )
    .expect("version eight backup should open");
    assert_eq!(pragma_integer(&backup, "PRAGMA user_version"), 8);
}

#[test]
fn schema_version_nine_migrates_to_current_version() {
    let root = TestRoot::new("schema-v9-migration");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let (initialization_path, file) = paths
        .create_database_initialization_file(&[0xc9; 16])
        .expect("version nine initialization file should be created");
    drop(file);
    let connection =
        Connection::open(&initialization_path).expect("version nine fixture should open");
    for schema in [
        include_str!("../schema_v1.sql"),
        include_str!("../schema_v2.sql"),
        include_str!("../schema_v3.sql"),
        include_str!("../schema_v4.sql"),
        include_str!("../schema_v5.sql"),
        include_str!("../schema_v6.sql"),
        include_str!("../schema_v7.sql"),
        include_str!("../schema_v8.sql"),
        include_str!("../schema_v9.sql"),
    ] {
        connection
            .execute_batch(schema)
            .expect("schema fixture should migrate");
    }
    drop(connection);
    paths
        .install_database(&initialization_path)
        .expect("version nine database should install");
    let connection = database::open(&paths).expect("version nine database should migrate");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 25);
    let mode_version: String = connection
        .query_row(
            "SELECT name FROM pragma_table_info('repository_import_requests')
             WHERE name = 'review_baseline_version'",
            [],
            |row| row.get(0),
        )
        .expect("baseline mode version column should exist");
    assert_eq!(mode_version, "review_baseline_version");
    let backup = Connection::open(
        root.path()
            .join("backups/sessions-before-schema-v9.sqlite3"),
    )
    .expect("version nine backup should open");
    assert_eq!(pragma_integer(&backup, "PRAGMA user_version"), 9);
}

#[test]
fn schema_version_ten_migrates_to_current_version() {
    let root = TestRoot::new("schema-v10-migration");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let (initialization_path, file) = paths
        .create_database_initialization_file(&[0xca; 16])
        .expect("version ten initialization file should be created");
    drop(file);
    let connection =
        Connection::open(&initialization_path).expect("version ten fixture should open");
    for schema in [
        include_str!("../schema_v1.sql"),
        include_str!("../schema_v2.sql"),
        include_str!("../schema_v3.sql"),
        include_str!("../schema_v4.sql"),
        include_str!("../schema_v5.sql"),
        include_str!("../schema_v6.sql"),
        include_str!("../schema_v7.sql"),
        include_str!("../schema_v8.sql"),
        include_str!("../schema_v9.sql"),
        include_str!("../schema_v10.sql"),
    ] {
        connection
            .execute_batch(schema)
            .expect("schema fixture should migrate");
    }
    drop(connection);
    paths
        .install_database(&initialization_path)
        .expect("version ten database should install");
    let connection = database::open(&paths).expect("version ten database should migrate");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 25);
    for table in [
        "session_creation_requests",
        "session_created_facts",
        "sessions",
    ] {
        let column: String = connection
            .query_row(
                "SELECT name FROM pragma_table_info(?1) WHERE name = 'working_directory'",
                [table],
                |row| row.get(0),
            )
            .expect("working directory column should exist");
        assert_eq!(column, "working_directory");
    }
    let backup = Connection::open(
        root.path()
            .join("backups/sessions-before-schema-v10.sqlite3"),
    )
    .expect("version ten backup should open");
    assert_eq!(pragma_integer(&backup, "PRAGMA user_version"), 10);
}

#[test]
fn schema_version_twelve_migrates_to_current_version() {
    let root = TestRoot::new("schema-v12-migration");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let (initialization_path, file) = paths
        .create_database_initialization_file(&[0xcb; 16])
        .expect("version twelve initialization file should be created");
    drop(file);
    let connection =
        Connection::open(&initialization_path).expect("version twelve fixture should open");
    for schema in [
        include_str!("../schema_v1.sql"),
        include_str!("../schema_v2.sql"),
        include_str!("../schema_v3.sql"),
        include_str!("../schema_v4.sql"),
        include_str!("../schema_v5.sql"),
        include_str!("../schema_v6.sql"),
        include_str!("../schema_v7.sql"),
        include_str!("../schema_v8.sql"),
        include_str!("../schema_v9.sql"),
        include_str!("../schema_v10.sql"),
        include_str!("../schema_v11.sql"),
        include_str!("../schema_v12.sql"),
    ] {
        connection
            .execute_batch(schema)
            .expect("schema fixture should migrate");
    }
    drop(connection);
    paths
        .install_database(&initialization_path)
        .expect("version twelve database should install");
    let connection = database::open(&paths).expect("version twelve database should migrate");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 25);
    let sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'tool_calls'",
            [],
            |row| row.get(0),
        )
        .expect("tool call schema should exist");
    assert!(sql.contains("tool_kind BETWEEN 1 AND 14"));
    let backup = Connection::open(
        root.path()
            .join("backups/sessions-before-schema-v12.sqlite3"),
    )
    .expect("version twelve backup should open");
    assert_eq!(pragma_integer(&backup, "PRAGMA user_version"), 12);
}

#[test]
fn schema_version_twenty_three_migrates_to_current_version() {
    let root = TestRoot::new("migration-v23-v24");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    drop(store);

    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection = Connection::open(&database_path).expect("database should open for downgrade");
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             PRAGMA defer_foreign_keys = ON;
             DROP INDEX subagent_model_selections_by_sequence;
             DROP TABLE subagent_model_selections;
             DROP INDEX default_model_selections_by_sequence;
             DROP TABLE default_model_selections;
             CREATE TABLE mutation_requests_v23_fixture (
                request_id BLOB PRIMARY KEY NOT NULL CHECK (length(request_id) = 16),
                operation_kind INTEGER NOT NULL CHECK (operation_kind BETWEEN 1 AND 14),
                accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
                accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0)
             ) STRICT, WITHOUT ROWID;
             INSERT INTO mutation_requests_v23_fixture SELECT * FROM mutation_requests;
             DROP TABLE mutation_requests;
             ALTER TABLE mutation_requests_v23_fixture RENAME TO mutation_requests;
             PRAGMA user_version = 23;
             COMMIT;",
        )
        .expect("version 23 fixture should be created");
    drop(connection);

    let store = SessionStore::open_at(root.path()).expect("version 23 should migrate");
    drop(store);
    let connection = Connection::open(database_path).expect("migrated database should open");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 25);
    let default_model_table: String = connection
        .query_row(
            "SELECT name FROM sqlite_schema WHERE name = 'default_model_selections'",
            [],
            |row| row.get(0),
        )
        .expect("default model table should exist");
    assert_eq!(default_model_table, "default_model_selections");
}

#[test]
fn schema_version_twenty_four_migrates_to_current_version() {
    let root = TestRoot::new("migration-v24-v25");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    drop(store);

    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection = Connection::open(&database_path).expect("database should open for downgrade");
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             PRAGMA defer_foreign_keys = ON;
             DROP INDEX subagent_model_selections_by_sequence;
             DROP TABLE subagent_model_selections;
             CREATE TABLE mutation_requests_v24_fixture (
                request_id BLOB PRIMARY KEY NOT NULL CHECK (length(request_id) = 16),
                operation_kind INTEGER NOT NULL CHECK (operation_kind BETWEEN 1 AND 15),
                accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
                accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0)
             ) STRICT, WITHOUT ROWID;
             INSERT INTO mutation_requests_v24_fixture SELECT * FROM mutation_requests;
             DROP TABLE mutation_requests;
             ALTER TABLE mutation_requests_v24_fixture RENAME TO mutation_requests;
             PRAGMA user_version = 24;
             COMMIT;",
        )
        .expect("version 24 fixture should be created");
    drop(connection);

    let store = SessionStore::open_at(root.path()).expect("version 24 should migrate");
    drop(store);
    let connection = Connection::open(database_path).expect("migrated database should open");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 25);
    let table: String = connection
        .query_row(
            "SELECT name FROM sqlite_schema WHERE name = 'subagent_model_selections'",
            [],
            |row| row.get(0),
        )
        .expect("subagent model setting table should exist");
    assert_eq!(table, "subagent_model_selections");
}
