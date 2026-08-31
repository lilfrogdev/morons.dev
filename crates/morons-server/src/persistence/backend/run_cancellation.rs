use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    Backend,
    records::{
        MUTATION_OPERATION_RUN_CANCEL, current_time_milliseconds, load_mutation_operation,
        load_session, next_sequence, random_identifier, sequence_to_sql, time_to_sql,
    },
    run_acceptance::insert_delivery_event,
    run_records::{AUDIT_CANCELLATION_REQUESTED, EVENT_CANCELLATION_REQUESTED, load_scoped_run},
};
use crate::persistence::{
    MutationRequestId, PersistenceError, PersistenceResourceLimit, RunCancellationResult, RunId,
    SessionId, types::REQUEST_FINGERPRINT_BYTES,
};

const AUDIT_CANCELLATION_OBSERVED: i64 = 11;
const MAX_RUN_CANCELLATION_REQUESTS: i64 = 200_000;

struct CancellationRequest {
    fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
    run_id: RunId,
    state: crate::persistence::RunState,
    cancellation_requested: bool,
    intent_applied: bool,
}

impl CancellationRequest {
    const fn result(&self) -> RunCancellationResult {
        RunCancellationResult {
            run_id: self.run_id,
            state: self.state,
            cancellation_requested: self.cancellation_requested,
            intent_applied: self.intent_applied,
        }
    }
}

impl Backend {
    pub(crate) fn cancel_run(
        &mut self,
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        session_id: SessionId,
        run_id: RunId,
    ) -> Result<RunCancellationResult, PersistenceError> {
        if let Some(existing) =
            resolve_existing_cancellation(&self.connection, request_id, &fingerprint)?
        {
            return Ok(existing);
        }

        let delivery_event_id = random_identifier()?;
        let audit_id = random_identifier()?;
        let accepted_at_milliseconds = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) =
            resolve_existing_cancellation(&transaction, request_id, &fingerprint)?
        {
            transaction.rollback()?;
            return Ok(existing);
        }
        if load_session(&transaction, session_id)?.is_none() {
            return Err(PersistenceError::SessionNotFound);
        }
        let run = load_scoped_run(&transaction, session_id, run_id)?
            .ok_or(PersistenceError::RunNotFound)?;
        let cancellation_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM run_cancellation_requests",
            [],
            |row| row.get(0),
        )?;
        if cancellation_count >= MAX_RUN_CANCELLATION_REQUESTS {
            return Err(PersistenceError::ResourceLimit {
                resource: PersistenceResourceLimit::Runs,
            });
        }
        let intent_applied = !run.state.is_terminal() && !run.cancellation_requested;
        let cancellation_requested = run.cancellation_requested || intent_applied;
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
                MUTATION_OPERATION_RUN_CANCEL,
                sequence_to_sql(accepted_sequence)?,
                time_to_sql(accepted_at_milliseconds)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO run_cancellation_requests (
                request_id,
                operation_fingerprint,
                session_id,
                run_id,
                fact_sequence,
                accepted_at_milliseconds,
                result_state,
                result_cancellation_requested,
                intent_applied,
                delivery_event_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                &request_id.as_bytes()[..],
                &fingerprint[..],
                &session_id.as_bytes()[..],
                &run_id.as_bytes()[..],
                sequence_to_sql(accepted_sequence)?,
                time_to_sql(accepted_at_milliseconds)?,
                run.state.to_record(),
                cancellation_requested,
                intent_applied,
                intent_applied.then_some(&delivery_event_id[..]),
            ],
        )?;
        if intent_applied {
            transaction.execute(
                "UPDATE runs
                 SET cancellation_requested = 1,
                     updated_sequence = ?1,
                     updated_at_milliseconds = ?2
                 WHERE run_id = ?3",
                params![
                    sequence_to_sql(accepted_sequence)?,
                    time_to_sql(accepted_at_milliseconds)?,
                    &run_id.as_bytes()[..],
                ],
            )?;
            transaction.execute(
                "UPDATE session_run_states SET updated_sequence = ?1 WHERE session_id = ?2",
                params![
                    sequence_to_sql(accepted_sequence)?,
                    &session_id.as_bytes()[..]
                ],
            )?;
            transaction.execute(
                "UPDATE sessions SET updated_sequence = ?1 WHERE session_id = ?2",
                params![
                    sequence_to_sql(accepted_sequence)?,
                    &session_id.as_bytes()[..]
                ],
            )?;
            insert_delivery_event(
                &transaction,
                &delivery_event_id,
                accepted_sequence,
                session_id,
                EVENT_CANCELLATION_REQUESTED,
                accepted_at_milliseconds,
            )?;
        }
        transaction.execute(
            "INSERT INTO run_audit_facts (
                audit_id,
                audit_sequence,
                request_id,
                session_id,
                run_id,
                operation_id,
                actor_kind,
                audit_kind,
                created_at_milliseconds
             ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, 1, ?6, ?7)",
            params![
                &audit_id[..],
                sequence_to_sql(audit_sequence)?,
                &request_id.as_bytes()[..],
                &session_id.as_bytes()[..],
                &run_id.as_bytes()[..],
                if intent_applied {
                    AUDIT_CANCELLATION_REQUESTED
                } else {
                    AUDIT_CANCELLATION_OBSERVED
                },
                time_to_sql(accepted_at_milliseconds)?,
            ],
        )?;
        transaction.commit()?;
        Ok(RunCancellationResult {
            run_id,
            state: run.state,
            cancellation_requested,
            intent_applied,
        })
    }
}

fn resolve_existing_cancellation(
    connection: &rusqlite::Connection,
    request_id: MutationRequestId,
    fingerprint: &[u8; REQUEST_FINGERPRINT_BYTES],
) -> Result<Option<RunCancellationResult>, PersistenceError> {
    let request = load_cancellation_request(connection, request_id)?;
    let operation = load_mutation_operation(connection, request_id)?;
    match (request, operation) {
        (Some(request), Some(MUTATION_OPERATION_RUN_CANCEL)) => {
            if &request.fingerprint != fingerprint {
                return Err(PersistenceError::RequestConflict);
            }
            Ok(Some(request.result()))
        }
        (Some(_), _) => Err(PersistenceError::InvalidState {
            reason: "a run cancellation request is missing its mutation registry record",
        }),
        (None, Some(_)) => Err(PersistenceError::RequestConflict),
        (None, None) => Ok(None),
    }
}

fn load_cancellation_request(
    connection: &rusqlite::Connection,
    request_id: MutationRequestId,
) -> Result<Option<CancellationRequest>, PersistenceError> {
    connection
        .query_row(
            "SELECT
                operation_fingerprint,
                run_id,
                result_state,
                result_cancellation_requested,
                intent_applied
             FROM run_cancellation_requests
             WHERE request_id = ?1",
            [&request_id.as_bytes()[..]],
            |row| {
                Ok(CancellationRequest {
                    fingerprint: row.get(0)?,
                    run_id: RunId::from_bytes(row.get(1)?),
                    state: crate::persistence::RunState::from_record(row.get(2)?)?,
                    cancellation_requested: row.get(3)?,
                    intent_applied: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(PersistenceError::from)
}
