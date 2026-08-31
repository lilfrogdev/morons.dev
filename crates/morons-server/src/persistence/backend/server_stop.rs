use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    Backend,
    records::{
        MUTATION_OPERATION_SERVER_STOP, current_time_milliseconds, load_mutation_operation,
        next_sequence, random_identifier, sequence_to_sql, time_to_sql,
    },
};
use crate::persistence::{
    MutationRequestId, PersistenceError, ServerStopResult,
    types::{REQUEST_FINGERPRINT_BYTES, stop_server_fingerprint},
};

const AUDIT_KIND_SERVER_STOP_ACCEPTED: i64 = 1;

struct StoredServerStop {
    fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
    host_epoch: [u8; 16],
}

impl Backend {
    pub(crate) fn request_server_stop(
        &mut self,
        request_id: MutationRequestId,
        host_epoch: [u8; 16],
    ) -> Result<ServerStopResult, PersistenceError> {
        let fingerprint = stop_server_fingerprint();
        let mutation_operation = load_mutation_operation(&self.connection, request_id)?;
        match (
            load_server_stop(&self.connection, request_id)?,
            mutation_operation,
        ) {
            (Some(existing), Some(MUTATION_OPERATION_SERVER_STOP)) => {
                if existing.fingerprint != fingerprint {
                    return Err(PersistenceError::RequestConflict);
                }
                return Ok(ServerStopResult {
                    signal_current_supervisor: false,
                    accepted_host_epoch: existing.host_epoch,
                });
            }
            (Some(_), _) => {
                return Err(PersistenceError::InvalidState {
                    reason: "a server stop request is missing its mutation registry record",
                });
            }
            (None, Some(_)) => return Err(PersistenceError::RequestConflict),
            (None, None) => {}
        }

        let audit_id = random_identifier()?;
        let accepted_at_milliseconds = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let signal_already_applied: bool = transaction.query_row(
            "SELECT EXISTS (
                SELECT 1 FROM server_stop_requests
                WHERE host_epoch = ?1 AND signal_applied = 1
             )",
            [&host_epoch[..]],
            |row| row.get(0),
        )?;
        let signal_current_supervisor = !signal_already_applied;
        let accepted_sequence = next_sequence(&transaction)?;
        let audit_sequence = next_sequence(&transaction)?;
        transaction.execute(
            "INSERT INTO mutation_requests (
                request_id,
                operation_kind,
                accepted_sequence,
                accepted_at_milliseconds
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                &request_id.as_bytes()[..],
                MUTATION_OPERATION_SERVER_STOP,
                sequence_to_sql(accepted_sequence)?,
                time_to_sql(accepted_at_milliseconds)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO server_stop_requests (
                request_id,
                operation_fingerprint,
                host_epoch,
                signal_applied,
                accepted_sequence,
                accepted_at_milliseconds
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                &request_id.as_bytes()[..],
                &fingerprint[..],
                &host_epoch[..],
                signal_current_supervisor,
                sequence_to_sql(accepted_sequence)?,
                time_to_sql(accepted_at_milliseconds)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO server_audit_facts (
                audit_id,
                audit_sequence,
                request_id,
                host_epoch,
                audit_kind,
                created_at_milliseconds
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                &audit_id[..],
                sequence_to_sql(audit_sequence)?,
                &request_id.as_bytes()[..],
                &host_epoch[..],
                AUDIT_KIND_SERVER_STOP_ACCEPTED,
                time_to_sql(accepted_at_milliseconds)?,
            ],
        )?;
        transaction.commit()?;
        Ok(ServerStopResult {
            signal_current_supervisor,
            accepted_host_epoch: host_epoch,
        })
    }
}

fn load_server_stop(
    connection: &rusqlite::Connection,
    request_id: MutationRequestId,
) -> Result<Option<StoredServerStop>, PersistenceError> {
    connection
        .query_row(
            "SELECT operation_fingerprint, host_epoch
             FROM server_stop_requests
             WHERE request_id = ?1",
            [&request_id.as_bytes()[..]],
            |row| {
                Ok(StoredServerStop {
                    fingerprint: row.get(0)?,
                    host_epoch: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(PersistenceError::from)
}
