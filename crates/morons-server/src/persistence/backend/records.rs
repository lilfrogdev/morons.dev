use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction};

use super::super::{
    MutationRequestId, PersistenceError, PersistenceResourceLimit, Session, SessionId,
    types::{IDENTIFIER_BYTES, REQUEST_FINGERPRINT_BYTES},
};

pub(super) const CREATION_STATE_PREPARED: i64 = 0;
pub(super) const CREATION_STATE_WORKSPACE_DISPATCHED: i64 = 1;
pub(super) const CREATION_STATE_READY: i64 = 2;

#[derive(Clone)]
pub(super) struct CreationRequest {
    pub(super) request_id: MutationRequestId,
    pub(super) fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
    pub(super) session_id: SessionId,
    pub(super) workspace_id: [u8; IDENTIFIER_BYTES],
    pub(super) display_name: Option<String>,
    pub(super) accepted_sequence: u64,
    pub(super) accepted_at_milliseconds: u64,
    pub(super) state: i64,
}

pub(super) fn load_creation_request(
    connection: &Connection,
    request_id: MutationRequestId,
) -> Result<Option<CreationRequest>, PersistenceError> {
    connection
        .query_row(
            "SELECT
                request_id,
                operation_fingerprint,
                session_id,
                workspace_id,
                display_name,
                accepted_sequence,
                accepted_at_milliseconds,
                state
            FROM session_creation_requests
            WHERE request_id = ?1",
            [&request_id.as_bytes()[..]],
            creation_request_from_row,
        )
        .optional()
        .map_err(PersistenceError::from)
}

pub(super) fn creation_request_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<CreationRequest> {
    Ok(CreationRequest {
        request_id: MutationRequestId::from_bytes(row.get(0)?),
        fingerprint: row.get(1)?,
        session_id: SessionId::from_bytes(row.get(2)?),
        workspace_id: row.get(3)?,
        display_name: row.get(4)?,
        accepted_sequence: nonnegative_integer_from_row(row, 5)?,
        accepted_at_milliseconds: nonnegative_integer_from_row(row, 6)?,
        state: row.get(7)?,
    })
}

pub(super) fn validate_request_retry(
    existing: &CreationRequest,
    expected_fingerprint: &[u8; REQUEST_FINGERPRINT_BYTES],
    expected_display_name: Option<&str>,
) -> Result<(), PersistenceError> {
    if &existing.fingerprint != expected_fingerprint
        || existing.display_name.as_deref() != expected_display_name
    {
        return Err(PersistenceError::RequestConflict);
    }
    Ok(())
}

pub(super) fn validate_creation_identity(
    current: &CreationRequest,
    expected: &CreationRequest,
) -> Result<(), PersistenceError> {
    validate_request_retry(
        current,
        &expected.fingerprint,
        expected.display_name.as_deref(),
    )?;
    if current.session_id != expected.session_id
        || current.workspace_id != expected.workspace_id
        || current.accepted_sequence != expected.accepted_sequence
        || current.accepted_at_milliseconds != expected.accepted_at_milliseconds
    {
        return Err(PersistenceError::InvalidState {
            reason: "a prepared session creation request changed identity",
        });
    }
    Ok(())
}

pub(super) fn next_sequence(transaction: &Transaction<'_>) -> Result<u64, PersistenceError> {
    let sequence = transaction
        .query_row(
            "UPDATE logical_sequences
             SET next_value = next_value + 1
             WHERE singleton = 1 AND next_value < 9223372036854775807
             RETURNING next_value - 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or(PersistenceError::ResourceLimit {
            resource: PersistenceResourceLimit::LogicalSequence,
        })?;
    u64::try_from(sequence).map_err(|_| PersistenceError::InvalidState {
        reason: "a logical sequence is outside its supported range",
    })
}

pub(super) fn load_session(
    connection: &Connection,
    session_id: SessionId,
) -> Result<Option<Session>, PersistenceError> {
    connection
        .query_row(
            "SELECT
                session_id,
                workspace_id,
                display_name,
                created_sequence,
                updated_sequence,
                created_at_milliseconds
            FROM sessions
            WHERE session_id = ?1",
            [&session_id.as_bytes()[..]],
            session_from_row,
        )
        .optional()
        .map_err(PersistenceError::from)
}

pub(super) fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: SessionId::from_bytes(row.get(0)?),
        workspace_id: row.get(1)?,
        display_name: row.get(2)?,
        created_sequence: nonnegative_integer_from_row(row, 3)?,
        updated_sequence: nonnegative_integer_from_row(row, 4)?,
        created_at_milliseconds: nonnegative_integer_from_row(row, 5)?,
    })
}

pub(super) fn sequence_to_sql(sequence: u64) -> Result<i64, PersistenceError> {
    i64::try_from(sequence).map_err(|_| PersistenceError::InvalidState {
        reason: "a logical sequence exceeds SQLite's integer range",
    })
}

pub(super) fn time_to_sql(milliseconds: u64) -> Result<i64, PersistenceError> {
    i64::try_from(milliseconds).map_err(|_| PersistenceError::InvalidState {
        reason: "a timestamp exceeds SQLite's integer range",
    })
}

pub(super) fn current_time_milliseconds() -> Result<u64, PersistenceError> {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| PersistenceError::InvalidState {
            reason: "the system clock is before the Unix epoch",
        })?
        .as_millis();
    u64::try_from(milliseconds).map_err(|_| PersistenceError::InvalidState {
        reason: "the system clock exceeds the supported timestamp range",
    })
}

pub(super) fn random_identifier() -> Result<[u8; IDENTIFIER_BYTES], PersistenceError> {
    let mut bytes = [0_u8; IDENTIFIER_BYTES];
    getrandom::fill(&mut bytes)?;
    Ok(bytes)
}

fn nonnegative_integer_from_row(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value = row.get::<_, i64>(index)?;
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}
