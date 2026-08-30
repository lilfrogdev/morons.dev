use rusqlite::{Connection, config::DbConfig, limits::Limit};

use crate::persistence::PersistenceError;

const EXPECTED_SQLITE_VERSION: &str = "3.53.2";
const PAGE_SIZE_BYTES: i64 = 4096;
const MAX_DATABASE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_PAGE_COUNT: i64 = MAX_DATABASE_BYTES as i64 / PAGE_SIZE_BYTES;
const MAX_SQLITE_VALUE_BYTES: i32 = 16 * 1024 * 1024;
const MAX_SQL_BYTES: i32 = 128 * 1024;
const MAX_COLUMNS: i32 = 128;
const MAX_EXPRESSION_DEPTH: i32 = 100;
const MAX_COMPOUND_SELECTS: i32 = 16;
const MAX_VDBE_OPERATIONS: i32 = 100_000;
const MAX_FUNCTION_ARGUMENTS: i32 = 64;
const MAX_LIKE_PATTERN_BYTES: i32 = 4096;
const MAX_VARIABLES: i32 = 1024;
const MAX_PARSER_DEPTH: i32 = 100;

pub(super) const MAXIMUM_DATABASE_BYTES: u64 = MAX_DATABASE_BYTES;

pub(super) fn configure(connection: &Connection, is_new: bool) -> Result<(), PersistenceError> {
    if rusqlite::version() != EXPECTED_SQLITE_VERSION {
        return Err(PersistenceError::InvalidState {
            reason: "the linked SQLite version is not the reviewed bundled version",
        });
    }

    connection.load_extension_disable()?;
    set_limit(
        connection,
        Limit::SQLITE_LIMIT_LENGTH,
        MAX_SQLITE_VALUE_BYTES,
    )?;
    set_limit(connection, Limit::SQLITE_LIMIT_SQL_LENGTH, MAX_SQL_BYTES)?;
    set_limit(connection, Limit::SQLITE_LIMIT_COLUMN, MAX_COLUMNS)?;
    set_limit(
        connection,
        Limit::SQLITE_LIMIT_EXPR_DEPTH,
        MAX_EXPRESSION_DEPTH,
    )?;
    set_limit(
        connection,
        Limit::SQLITE_LIMIT_COMPOUND_SELECT,
        MAX_COMPOUND_SELECTS,
    )?;
    set_limit(connection, Limit::SQLITE_LIMIT_VDBE_OP, MAX_VDBE_OPERATIONS)?;
    set_limit(
        connection,
        Limit::SQLITE_LIMIT_FUNCTION_ARG,
        MAX_FUNCTION_ARGUMENTS,
    )?;
    set_limit(connection, Limit::SQLITE_LIMIT_ATTACHED, 0)?;
    set_limit(
        connection,
        Limit::SQLITE_LIMIT_LIKE_PATTERN_LENGTH,
        MAX_LIKE_PATTERN_BYTES,
    )?;
    set_limit(
        connection,
        Limit::SQLITE_LIMIT_VARIABLE_NUMBER,
        MAX_VARIABLES,
    )?;
    set_limit(connection, Limit::SQLITE_LIMIT_TRIGGER_DEPTH, 0)?;
    set_limit(connection, Limit::SQLITE_LIMIT_WORKER_THREADS, 0)?;
    set_limit(
        connection,
        Limit::SQLITE_LIMIT_PARSER_DEPTH,
        MAX_PARSER_DEPTH,
    )?;

    set_db_config(connection, DbConfig::SQLITE_DBCONFIG_ENABLE_FKEY, true)?;
    set_db_config(connection, DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
    set_db_config(connection, DbConfig::SQLITE_DBCONFIG_DQS_DDL, false)?;
    set_db_config(connection, DbConfig::SQLITE_DBCONFIG_DQS_DML, false)?;
    set_db_config(
        connection,
        DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_CREATE,
        false,
    )?;
    set_db_config(
        connection,
        DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_WRITE,
        false,
    )?;
    set_db_config(connection, DbConfig::SQLITE_DBCONFIG_ENABLE_COMMENTS, false)?;

    if is_new {
        connection.pragma_update(None, "page_size", PAGE_SIZE_BYTES)?;
    }
    let journal_mode: String =
        connection.query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))?;
    if journal_mode != "delete" {
        return Err(PersistenceError::InvalidState {
            reason: "SQLite refused rollback journal mode",
        });
    }

    connection.pragma_update(None, "synchronous", "EXTRA")?;
    connection.pragma_update(None, "fullfsync", true)?;
    connection.pragma_update(None, "temp_store", "MEMORY")?;
    connection.pragma_update(None, "mmap_size", 0)?;
    connection.pragma_update(None, "busy_timeout", 0)?;
    connection.pragma_update(None, "cell_size_check", true)?;
    connection.pragma_update(None, "recursive_triggers", false)?;
    connection.pragma_update(None, "locking_mode", "NORMAL")?;
    let page_count_limit: i64 =
        connection.query_row("PRAGMA max_page_count = 262144", [], |row| row.get(0))?;
    if page_count_limit != MAX_PAGE_COUNT {
        return Err(PersistenceError::InvalidState {
            reason: "SQLite refused the database page-count limit",
        });
    }

    set_db_config(connection, DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    verify(connection)
}

fn verify(connection: &Connection) -> Result<(), PersistenceError> {
    verify_pragma_integer(connection, "PRAGMA page_size", PAGE_SIZE_BYTES)?;
    verify_pragma_integer(connection, "PRAGMA synchronous", 3)?;
    verify_pragma_integer(connection, "PRAGMA fullfsync", 1)?;
    verify_pragma_integer(connection, "PRAGMA foreign_keys", 1)?;
    verify_pragma_integer(connection, "PRAGMA trusted_schema", 0)?;
    verify_pragma_integer(connection, "PRAGMA temp_store", 2)?;
    verify_pragma_integer(connection, "PRAGMA mmap_size", 0)?;
    verify_pragma_integer(connection, "PRAGMA busy_timeout", 0)?;
    verify_pragma_integer(connection, "PRAGMA cell_size_check", 1)?;
    verify_pragma_integer(connection, "PRAGMA recursive_triggers", 0)?;

    let locking_mode: String = connection.query_row("PRAGMA locking_mode", [], |row| row.get(0))?;
    if locking_mode != "normal"
        || !connection.db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE)?
        || connection.db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA)?
        || !connection.db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_FKEY)?
        || connection.db_config(DbConfig::SQLITE_DBCONFIG_DQS_DDL)?
        || connection.db_config(DbConfig::SQLITE_DBCONFIG_DQS_DML)?
        || connection.db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_CREATE)?
        || connection.db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_WRITE)?
        || connection.db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_COMMENTS)?
    {
        return Err(PersistenceError::InvalidState {
            reason: "SQLite connection security configuration did not remain active",
        });
    }
    Ok(())
}

fn set_limit(connection: &Connection, limit: Limit, value: i32) -> Result<(), PersistenceError> {
    connection.set_limit(limit, value)?;
    if connection.limit(limit)? != value {
        return Err(PersistenceError::InvalidState {
            reason: "SQLite refused a required resource limit",
        });
    }
    Ok(())
}

fn set_db_config(
    connection: &Connection,
    config: DbConfig,
    value: bool,
) -> Result<(), PersistenceError> {
    if connection.set_db_config(config, value)? != value || connection.db_config(config)? != value {
        return Err(PersistenceError::InvalidState {
            reason: "SQLite refused a required connection configuration",
        });
    }
    Ok(())
}

fn verify_pragma_integer(
    connection: &Connection,
    statement: &'static str,
    expected: i64,
) -> Result<(), PersistenceError> {
    let actual: i64 = connection.query_row(statement, [], |row| row.get(0))?;
    if actual != expected {
        return Err(PersistenceError::InvalidState {
            reason: "SQLite refused a required pragma setting",
        });
    }
    Ok(())
}
