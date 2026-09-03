mod configuration;
mod projections;

use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{Connection, OpenFlags, backup::Backup};

use self::configuration::MAXIMUM_DATABASE_BYTES;
use super::{
    PersistenceError,
    paths::{StoragePaths, path_entry_exists},
};

const APPLICATION_ID: i64 = 1_297_044_046;
const SCHEMA_VERSION: i64 = 22;
const SQLITE_HEADER_BYTES: usize = 72;
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";
const APPLICATION_ID_OFFSET: usize = 68;
const SCHEMA_V1: &str = include_str!("../schema_v1.sql");
const SCHEMA_V2: &str = include_str!("../schema_v2.sql");
const SCHEMA_V3: &str = include_str!("../schema_v3.sql");
const SCHEMA_V4: &str = include_str!("../schema_v4.sql");
const SCHEMA_V5: &str = include_str!("../schema_v5.sql");
const SCHEMA_V6: &str = include_str!("../schema_v6.sql");
const SCHEMA_V7: &str = include_str!("../schema_v7.sql");
const SCHEMA_V8: &str = include_str!("../schema_v8.sql");
const SCHEMA_V9: &str = include_str!("../schema_v9.sql");
const SCHEMA_V10: &str = include_str!("../schema_v10.sql");
const SCHEMA_V11: &str = include_str!("../schema_v11.sql");
const SCHEMA_V12: &str = include_str!("../schema_v12.sql");
const SCHEMA_V13: &str = include_str!("../schema_v13.sql");
const SCHEMA_V14: &str = include_str!("../schema_v14.sql");
const SCHEMA_V15: &str = include_str!("../schema_v15.sql");
const SCHEMA_V16: &str = include_str!("../schema_v16.sql");
const SCHEMA_V17: &str = include_str!("../schema_v17.sql");
const SCHEMA_V18: &str = include_str!("../schema_v18.sql");
const SCHEMA_V19: &str = include_str!("../schema_v19.sql");
const SCHEMA_V20: &str = include_str!("../schema_v20.sql");
const SCHEMA_V21: &str = include_str!("../schema_v21.sql");
const SCHEMA_V22: &str = include_str!("../schema_v22.sql");

const EXPECTED_SCHEMA_OBJECTS: &[(&str, &str)] = &[
    ("active_worktree_generations", "table"),
    ("audit_facts", "table"),
    ("credential_audit_facts", "table"),
    ("credential_mutation_requests", "table"),
    ("credential_mutation_requests_by_state", "index"),
    ("compaction_operations", "table"),
    ("context_checkpoints", "table"),
    ("context_checkpoints_by_session", "index"),
    ("credential_operation_facts", "table"),
    ("delivery_events", "table"),
    ("delivery_events_by_session", "index"),
    ("deleted_mutation_tombstones", "table"),
    ("current_execution_image", "table"),
    ("execution_image_audit_facts", "table"),
    ("execution_image_facts", "table"),
    ("execution_image_requests", "table"),
    ("execution_image_requests_by_state", "index"),
    ("execution_image_single_incomplete", "index"),
    ("image_attachments", "table"),
    ("image_attachments_by_message", "index"),
    ("local_command_audit_by_session", "index"),
    ("local_command_audit_facts", "table"),
    ("local_command_cancellations", "table"),
    ("local_commands", "table"),
    ("local_commands_by_session_entry", "index"),
    ("local_commands_one_active_per_session", "index"),
    ("logical_sequences", "table"),
    ("mutation_requests", "table"),
    ("provider_operation_facts", "table"),
    ("provider_operation_facts_by_run", "index"),
    ("repository_import_active_session", "index"),
    ("repository_import_audit_facts", "table"),
    ("repository_import_facts", "table"),
    ("repository_import_facts_by_session", "index"),
    ("repository_import_requests", "table"),
    ("repository_import_requests_by_state", "index"),
    ("run_accepted_facts", "table"),
    ("run_accepted_checkpoints", "table"),
    ("run_accepted_facts_by_session", "index"),
    ("run_audit_facts", "table"),
    ("run_audit_facts_by_operation", "index"),
    ("run_audit_facts_by_run", "index"),
    ("run_cancellation_intent_by_run", "index"),
    ("run_cancellation_requests", "table"),
    ("run_input_requests", "table"),
    ("run_state_facts", "table"),
    ("run_state_facts_by_run", "index"),
    ("run_skill_snapshots", "table"),
    ("runs", "table"),
    ("server_audit_facts", "table"),
    ("server_stop_requests", "table"),
    ("server_stop_signal_by_host_epoch", "index"),
    ("runs_by_session", "index"),
    ("session_created_facts", "table"),
    ("session_creation_requests", "table"),
    ("session_creation_requests_by_state", "index"),
    ("session_delete_attachments", "table"),
    ("session_delete_requests", "table"),
    ("session_entries", "table"),
    ("session_entries_by_session", "index"),
    ("session_entries_final_assistant_by_run", "index"),
    ("session_entries_tool_call_kind", "index"),
    ("session_entries_user_by_run", "index"),
    ("session_rename_requests", "table"),
    ("session_rename_requests_by_session", "index"),
    ("session_archive_requests", "table"),
    ("session_archive_requests_by_session", "index"),
    ("session_run_states", "table"),
    ("sessions", "table"),
    ("sessions_by_creation", "index"),
    ("tool_audit_facts", "table"),
    ("tool_audit_facts_by_run", "index"),
    ("tool_image_attachments", "table"),
    ("tool_image_attachments_by_run", "index"),
    ("tool_calls", "table"),
    ("tool_calls_by_run", "index"),
    ("tool_operation_facts", "table"),
    ("tool_operation_facts_by_run", "index"),
    ("tool_operation_terminal_by_call", "index"),
    ("tool_uncertainty_acknowledgements", "table"),
    ("workspace_generation_layout_active", "index"),
    ("workspace_generation_layouts", "table"),
    ("workspace_generation_layouts_by_state", "index"),
    ("workspace_operation_facts", "table"),
    ("worktree_generation_facts", "table"),
    ("worktree_generation_facts_by_workspace", "index"),
];

pub(crate) fn open(paths: &StoragePaths) -> Result<Connection, PersistenceError> {
    if !paths.database_exists()? {
        initialize(paths)?;
    }
    paths.validate_database_file(MAXIMUM_DATABASE_BYTES)?;
    validate_header(paths.database_path())?;

    let mut connection = open_connection(paths.database_path())?;
    configuration::configure(&connection, false)?;
    migrate(&connection, paths)?;
    validate_identity_and_schema(&connection)?;
    validate_quick_integrity(&connection)?;
    projections::repair(&mut connection)?;
    Ok(connection)
}

fn initialize(paths: &StoragePaths) -> Result<(), PersistenceError> {
    let nonce = random_identifier()?;
    let (initialization_path, file) = paths.create_database_initialization_file(&nonce)?;
    file.sync_all()?;
    drop(file);

    let result = initialize_at_path(paths, &initialization_path);
    if let Err(error) = result {
        remove_initialization_artifacts(paths, &initialization_path)?;
        return Err(error);
    }
    Ok(())
}

fn initialize_at_path(
    paths: &StoragePaths,
    initialization_path: &Path,
) -> Result<(), PersistenceError> {
    let connection = open_connection(initialization_path)?;
    configuration::configure(&connection, true)?;
    connection.execute_batch(SCHEMA_V1)?;
    connection.execute_batch(SCHEMA_V2)?;
    connection.execute_batch(SCHEMA_V3)?;
    connection.execute_batch(SCHEMA_V4)?;
    connection.execute_batch(SCHEMA_V5)?;
    connection.execute_batch(SCHEMA_V6)?;
    connection.execute_batch(SCHEMA_V7)?;
    connection.execute_batch(SCHEMA_V8)?;
    connection.execute_batch(SCHEMA_V9)?;
    connection.execute_batch(SCHEMA_V10)?;
    connection.execute_batch(SCHEMA_V11)?;
    connection.execute_batch(SCHEMA_V12)?;
    connection.execute_batch(SCHEMA_V13)?;
    connection.execute_batch(SCHEMA_V14)?;
    connection.execute_batch(SCHEMA_V15)?;
    connection.execute_batch(SCHEMA_V16)?;
    connection.execute_batch(SCHEMA_V17)?;
    connection.execute_batch(SCHEMA_V18)?;
    connection.execute_batch(SCHEMA_V19)?;
    connection.execute_batch(SCHEMA_V20)?;
    connection.execute_batch(SCHEMA_V21)?;
    connection.execute_batch(SCHEMA_V22)?;
    validate_identity_and_schema(&connection)?;
    validate_integrity(&connection)?;
    drop(connection);

    paths.validate_database_file_at(initialization_path, MAXIMUM_DATABASE_BYTES)?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(initialization_path)?
        .sync_all()?;
    paths
        .install_database(initialization_path)
        .map_err(PersistenceError::from)
}

fn remove_initialization_artifacts(
    paths: &StoragePaths,
    initialization_path: &Path,
) -> Result<(), PersistenceError> {
    let mut journal_name = OsString::from(initialization_path.as_os_str());
    journal_name.push("-journal");
    let journal_path = PathBuf::from(journal_name);
    paths.remove_initialization_file(&journal_path)?;
    paths.remove_initialization_file(initialization_path)?;
    Ok(())
}

fn open_connection(path: &Path) -> Result<Connection, PersistenceError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE;
    Ok(Connection::open_with_flags(path, flags)?)
}

fn validate_header(path: &Path) -> Result<(), PersistenceError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.len() < SQLITE_HEADER_BYTES as u64 {
        return Err(PersistenceError::InvalidState {
            reason: "the authoritative database header is truncated",
        });
    }

    let mut header = [0_u8; SQLITE_HEADER_BYTES];
    File::open(path)?.read_exact(&mut header)?;
    if &header[..SQLITE_MAGIC.len()] != SQLITE_MAGIC {
        return Err(PersistenceError::InvalidState {
            reason: "the authoritative database has an invalid file signature",
        });
    }
    let application_id = i64::from(u32::from_be_bytes(
        header[APPLICATION_ID_OFFSET..APPLICATION_ID_OFFSET + 4]
            .try_into()
            .map_err(|_| PersistenceError::InvalidState {
                reason: "the authoritative database application identifier is malformed",
            })?,
    ));
    if application_id != APPLICATION_ID {
        return Err(PersistenceError::InvalidState {
            reason: "the authoritative database belongs to another application",
        });
    }
    Ok(())
}

fn migrate(connection: &Connection, paths: &StoragePaths) -> Result<(), PersistenceError> {
    let application_id: i64 =
        connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    if application_id != APPLICATION_ID {
        return Err(PersistenceError::InvalidState {
            reason: "the authoritative database application identifier changed",
        });
    }

    let schema_version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if schema_version == SCHEMA_VERSION {
        return Ok(());
    }
    if schema_version > SCHEMA_VERSION {
        return Err(PersistenceError::InvalidState {
            reason: "the authoritative database schema is newer than this server",
        });
    }
    if !(1..SCHEMA_VERSION).contains(&schema_version) {
        return Err(PersistenceError::InvalidState {
            reason: "the authoritative database schema version is unsupported",
        });
    }
    ensure_migration_backup(connection, paths, schema_version)?;
    if schema_version == 1 {
        connection.execute_batch(SCHEMA_V2)?;
    }
    for (version, schema) in [
        (3, SCHEMA_V3),
        (4, SCHEMA_V4),
        (5, SCHEMA_V5),
        (6, SCHEMA_V6),
        (7, SCHEMA_V7),
        (8, SCHEMA_V8),
        (9, SCHEMA_V9),
        (10, SCHEMA_V10),
        (11, SCHEMA_V11),
        (12, SCHEMA_V12),
        (13, SCHEMA_V13),
        (14, SCHEMA_V14),
        (15, SCHEMA_V15),
        (16, SCHEMA_V16),
        (17, SCHEMA_V17),
        (18, SCHEMA_V18),
        (19, SCHEMA_V19),
        (20, SCHEMA_V20),
        (21, SCHEMA_V21),
        (22, SCHEMA_V22),
    ] {
        if version > schema_version {
            migrate_schema(connection, schema)?;
        }
    }
    Ok(())
}

fn ensure_migration_backup(
    source: &Connection,
    paths: &StoragePaths,
    source_version: i64,
) -> Result<(), PersistenceError> {
    validate_integrity(source)?;
    let final_path = paths.migration_backup_path(source_version)?;
    if path_entry_exists(&final_path)? {
        paths.validate_migration_backup_file(&final_path, MAXIMUM_DATABASE_BYTES)?;
        return validate_migration_backup_snapshot(&final_path, source_version);
    }

    let nonce = random_identifier()?;
    let (temporary_path, file) = paths.create_migration_backup_file(source_version, &nonce)?;
    file.sync_all()?;
    drop(file);
    let result = (|| -> Result<(), PersistenceError> {
        let mut destination = Connection::open(&temporary_path)?;
        let backup = Backup::new(source, &mut destination)?;
        backup.run_to_completion(128, Duration::ZERO, None)?;
        drop(backup);
        drop(destination);
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&temporary_path)?
            .sync_all()?;
        paths.validate_migration_backup_temporary_file(&temporary_path, MAXIMUM_DATABASE_BYTES)?;
        validate_migration_backup_snapshot(&temporary_path, source_version)?;
        let installed = paths.install_migration_backup(&temporary_path, source_version)?;
        paths.remove_migration_backup_temporary_file(&temporary_path)?;
        paths.validate_migration_backup_file(&installed, MAXIMUM_DATABASE_BYTES)?;
        validate_migration_backup_snapshot(&installed, source_version)
    })();
    if result.is_err() {
        let _ = paths.remove_migration_backup_temporary_file(&temporary_path);
    }
    result
}

fn validate_migration_backup_snapshot(
    path: &Path,
    expected_schema_version: i64,
) -> Result<(), PersistenceError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE;
    let connection = Connection::open_with_flags(path, flags)?;
    let application_id: i64 =
        connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    let schema_version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if application_id != APPLICATION_ID || schema_version != expected_schema_version {
        return Err(PersistenceError::InvalidState {
            reason: "a migration backup has invalid database identity",
        });
    }
    validate_integrity(&connection)
}

fn migrate_schema(connection: &Connection, schema: &str) -> Result<(), PersistenceError> {
    connection.pragma_update(None, "foreign_keys", false)?;
    let disabled: i64 = connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    if disabled != 0 {
        return Err(PersistenceError::InvalidState {
            reason: "foreign-key enforcement could not be suspended for migration",
        });
    }
    let migration = connection.execute_batch(schema);
    if migration.is_err() {
        let _ = connection.execute_batch("ROLLBACK");
    }
    let foreign_keys = connection.pragma_update(None, "foreign_keys", true);
    if let Err(error) = migration {
        foreign_keys?;
        return Err(PersistenceError::Sqlite(error));
    }
    foreign_keys?;
    let enabled: i64 = connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    if enabled != 1 {
        return Err(PersistenceError::InvalidState {
            reason: "foreign-key enforcement was not restored after migration",
        });
    }
    Ok(())
}

fn validate_identity_and_schema(connection: &Connection) -> Result<(), PersistenceError> {
    let application_id: i64 =
        connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    if application_id != APPLICATION_ID {
        return Err(PersistenceError::InvalidState {
            reason: "the authoritative database application identifier changed",
        });
    }

    let schema_version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if schema_version > SCHEMA_VERSION {
        return Err(PersistenceError::InvalidState {
            reason: "the authoritative database schema is newer than this server",
        });
    }
    if schema_version != SCHEMA_VERSION {
        return Err(PersistenceError::InvalidState {
            reason: "the authoritative database schema version is unsupported",
        });
    }

    let mut statement = connection.prepare(
        "SELECT name, type FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let actual = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<BTreeSet<_>, _>>()?;
    let expected = EXPECTED_SCHEMA_OBJECTS
        .iter()
        .map(|(name, kind)| ((*name).to_owned(), (*kind).to_owned()))
        .collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(PersistenceError::InvalidState {
            reason: "the authoritative database schema objects are invalid",
        });
    }
    Ok(())
}

fn validate_quick_integrity(connection: &Connection) -> Result<(), PersistenceError> {
    let quick_check: String =
        connection.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    if quick_check != "ok" {
        return Err(PersistenceError::InvalidState {
            reason: "the authoritative database failed its integrity check",
        });
    }
    Ok(())
}

pub(super) fn validate_integrity(connection: &Connection) -> Result<(), PersistenceError> {
    validate_quick_integrity(connection)?;
    let mut statement = connection.prepare("PRAGMA foreign_key_check")?;
    if statement.query([])?.next()?.is_some() {
        return Err(PersistenceError::InvalidState {
            reason: "the authoritative database failed its foreign-key check",
        });
    }
    Ok(())
}

fn random_identifier() -> Result<[u8; 16], PersistenceError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)?;
    Ok(bytes)
}
