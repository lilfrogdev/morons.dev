mod configuration;
mod projections;

use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OpenFlags};

use self::configuration::MAXIMUM_DATABASE_BYTES;
use super::{PersistenceError, paths::StoragePaths};

const APPLICATION_ID: i64 = 1_297_044_046;
const SCHEMA_VERSION: i64 = 1;
const SQLITE_HEADER_BYTES: usize = 72;
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";
const APPLICATION_ID_OFFSET: usize = 68;
const SCHEMA_V1: &str = include_str!("../schema_v1.sql");

const EXPECTED_SCHEMA_OBJECTS: &[(&str, &str)] = &[
    ("audit_facts", "table"),
    ("delivery_events", "table"),
    ("delivery_events_by_session", "index"),
    ("logical_sequences", "table"),
    ("session_created_facts", "table"),
    ("session_creation_requests", "table"),
    ("session_creation_requests_by_state", "index"),
    ("sessions", "table"),
    ("sessions_by_creation", "index"),
    ("workspace_operation_facts", "table"),
];

pub(crate) fn open(paths: &StoragePaths) -> Result<Connection, PersistenceError> {
    if !paths.database_exists()? {
        initialize(paths)?;
    }
    paths.validate_database_file(MAXIMUM_DATABASE_BYTES)?;
    validate_header(paths.database_path())?;

    let mut connection = open_connection(paths.database_path())?;
    configuration::configure(&connection, false)?;
    validate_identity_and_schema(&connection)?;
    validate_integrity(&connection)?;
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

pub(super) fn validate_integrity(connection: &Connection) -> Result<(), PersistenceError> {
    let quick_check: String =
        connection.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    if quick_check != "ok" {
        return Err(PersistenceError::InvalidState {
            reason: "the authoritative database failed its integrity check",
        });
    }

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
