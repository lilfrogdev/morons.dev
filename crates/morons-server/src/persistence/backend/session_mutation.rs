use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    Backend,
    records::{
        MUTATION_OPERATION_SESSION_RENAME, current_time_milliseconds, load_mutation_operation,
        next_sequence, random_identifier, sequence_to_sql, time_to_sql,
    },
};
use crate::persistence::{
    MutationRequestId, PersistenceError, Session, SessionId, types::REQUEST_FINGERPRINT_BYTES,
};

const EVENT_SESSION_RENAMED: i64 = 18;

impl Backend {
    pub(crate) fn rename_session(
        &mut self,
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        session_id: SessionId,
        display_name: String,
    ) -> Result<Session, PersistenceError> {
        let operation = load_mutation_operation(&self.connection, request_id)?;
        let existing = self
            .connection
            .query_row(
                "SELECT operation_fingerprint, session_id, display_name
                 FROM session_rename_requests WHERE request_id = ?1",
                [&request_id.as_bytes()[..]],
                |row| {
                    Ok((
                        row.get::<_, [u8; 32]>(0)?,
                        row.get::<_, [u8; 16]>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        match (operation, existing) {
            (Some(MUTATION_OPERATION_SESSION_RENAME), Some(existing)) => {
                if existing.0 != fingerprint
                    || existing.1 != *session_id.as_bytes()
                    || existing.2 != display_name
                {
                    return Err(PersistenceError::RequestConflict);
                }
                return self.load_required_session(session_id);
            }
            (Some(_), _) | (None, Some(_)) => return Err(PersistenceError::RequestConflict),
            (None, None) => {}
        }
        if self.get_session(session_id)?.is_none() {
            return Err(PersistenceError::SessionNotFound);
        }

        let now = current_time_milliseconds()?;
        let event_id = random_identifier()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sequence = next_sequence(&transaction)?;
        transaction.execute(
            "INSERT INTO mutation_requests (
                request_id, operation_kind, accepted_sequence, accepted_at_milliseconds
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                &request_id.as_bytes()[..],
                MUTATION_OPERATION_SESSION_RENAME,
                sequence_to_sql(sequence)?,
                time_to_sql(now)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO session_rename_requests (
                request_id, operation_fingerprint, session_id, display_name,
                accepted_sequence, accepted_at_milliseconds, delivery_event_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                &request_id.as_bytes()[..],
                &fingerprint[..],
                &session_id.as_bytes()[..],
                &display_name,
                sequence_to_sql(sequence)?,
                time_to_sql(now)?,
                &event_id[..],
            ],
        )?;
        transaction.execute(
            "INSERT INTO delivery_events (
                event_id, event_sequence, session_id, event_kind,
                payload_version, created_at_milliseconds
             ) VALUES (?1, ?2, ?3, ?4, 1, ?5)",
            params![
                &event_id[..],
                sequence_to_sql(sequence)?,
                &session_id.as_bytes()[..],
                EVENT_SESSION_RENAMED,
                time_to_sql(now)?,
            ],
        )?;
        let changed = transaction.execute(
            "UPDATE sessions
             SET display_name = ?1, updated_sequence = MAX(updated_sequence, ?2)
             WHERE session_id = ?3",
            params![
                &display_name,
                sequence_to_sql(sequence)?,
                &session_id.as_bytes()[..]
            ],
        )?;
        if changed != 1 {
            return Err(PersistenceError::InvalidState {
                reason: "a session rename lost its current-state projection",
            });
        }
        transaction.commit()?;
        self.load_required_session(session_id)
    }
}
