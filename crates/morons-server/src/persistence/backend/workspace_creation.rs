use rusqlite::{TransactionBehavior, params};

use super::{
    Backend,
    records::{
        CREATION_STATE_PREPARED, CREATION_STATE_READY, CREATION_STATE_WORKSPACE_DISPATCHED,
        CreationRequest, current_time_milliseconds, load_creation_request, next_sequence,
        random_identifier, sequence_to_sql, time_to_sql, validate_creation_identity,
    },
};
use crate::persistence::{PersistenceError, Session};

const WORKSPACE_OPERATION_DISPATCHED: i64 = 1;
const WORKSPACE_OPERATION_COMPLETED: i64 = 2;
const AUDIT_KIND_WORKSPACE_DISPATCHED: i64 = 2;
const AUDIT_KIND_SESSION_CREATE_COMPLETED: i64 = 3;

impl Backend {
    pub(super) fn dispatch_workspace_creation(
        &mut self,
        expected: &CreationRequest,
    ) -> Result<CreationRequest, PersistenceError> {
        let fact_id = random_identifier()?;
        let audit_id = random_identifier()?;
        let dispatched_at_milliseconds = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = load_creation_request(&transaction, expected.request_id)?.ok_or(
            PersistenceError::InvalidState {
                reason: "a prepared session creation request disappeared",
            },
        )?;
        validate_creation_identity(&current, expected)?;
        if current.state == CREATION_STATE_READY
            || current.state == CREATION_STATE_WORKSPACE_DISPATCHED
        {
            transaction.commit()?;
            return Ok(current);
        }
        if current.state != CREATION_STATE_PREPARED {
            return Err(PersistenceError::InvalidState {
                reason: "a session creation request has an unknown pre-dispatch state",
            });
        }

        let fact_sequence = next_sequence(&transaction)?;
        let audit_sequence = next_sequence(&transaction)?;
        transaction.execute(
            "INSERT INTO workspace_operation_facts (
                fact_id,
                fact_sequence,
                request_id,
                workspace_id,
                operation_kind,
                created_at_milliseconds
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                &fact_id[..],
                sequence_to_sql(fact_sequence)?,
                &current.request_id.as_bytes()[..],
                &current.workspace_id[..],
                WORKSPACE_OPERATION_DISPATCHED,
                time_to_sql(dispatched_at_milliseconds)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO audit_facts (
                audit_id,
                audit_sequence,
                request_id,
                session_id,
                audit_kind,
                created_at_milliseconds
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                &audit_id[..],
                sequence_to_sql(audit_sequence)?,
                &current.request_id.as_bytes()[..],
                &current.session_id.as_bytes()[..],
                AUDIT_KIND_WORKSPACE_DISPATCHED,
                time_to_sql(dispatched_at_milliseconds)?,
            ],
        )?;
        let updated = transaction.execute(
            "UPDATE session_creation_requests SET state = ?2 WHERE request_id = ?1 AND state = ?3",
            params![
                &current.request_id.as_bytes()[..],
                CREATION_STATE_WORKSPACE_DISPATCHED,
                CREATION_STATE_PREPARED,
            ],
        )?;
        if updated != 1 {
            return Err(PersistenceError::InvalidState {
                reason: "a session workspace dispatch did not reach its durable state",
            });
        }
        transaction.commit()?;

        Ok(CreationRequest {
            state: CREATION_STATE_WORKSPACE_DISPATCHED,
            ..current
        })
    }

    pub(super) fn finalize_session_creation(
        &mut self,
        expected: &CreationRequest,
    ) -> Result<Session, PersistenceError> {
        let workspace_fact_id = random_identifier()?;
        let session_fact_id = random_identifier()?;
        let delivery_event_id = random_identifier()?;
        let audit_id = random_identifier()?;
        let completed_at_milliseconds = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = load_creation_request(&transaction, expected.request_id)?.ok_or(
            PersistenceError::InvalidState {
                reason: "a prepared session creation request disappeared",
            },
        )?;
        validate_creation_identity(&current, expected)?;
        if current.state == CREATION_STATE_READY {
            transaction.commit()?;
            return self.load_required_session(current.session_id);
        }
        if current.state != CREATION_STATE_WORKSPACE_DISPATCHED {
            return Err(PersistenceError::InvalidState {
                reason: "a session creation request has an unknown completion state",
            });
        }

        let workspace_fact_sequence = next_sequence(&transaction)?;
        let session_fact_sequence = next_sequence(&transaction)?;
        let audit_sequence = next_sequence(&transaction)?;
        transaction.execute(
            "INSERT INTO workspace_operation_facts (
                fact_id,
                fact_sequence,
                request_id,
                workspace_id,
                operation_kind,
                created_at_milliseconds
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                &workspace_fact_id[..],
                sequence_to_sql(workspace_fact_sequence)?,
                &current.request_id.as_bytes()[..],
                &current.workspace_id[..],
                WORKSPACE_OPERATION_COMPLETED,
                time_to_sql(completed_at_milliseconds)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO session_created_facts (
                fact_id,
                fact_sequence,
                request_id,
                session_id,
                workspace_id,
                display_name,
                accepted_sequence,
                created_at_milliseconds,
                delivery_event_id
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                &session_fact_id[..],
                sequence_to_sql(session_fact_sequence)?,
                &current.request_id.as_bytes()[..],
                &current.session_id.as_bytes()[..],
                &current.workspace_id[..],
                current.display_name.as_deref(),
                sequence_to_sql(current.accepted_sequence)?,
                time_to_sql(current.accepted_at_milliseconds)?,
                &delivery_event_id[..],
            ],
        )?;
        transaction.execute(
            "INSERT INTO sessions (
                session_id,
                workspace_id,
                display_name,
                created_sequence,
                updated_sequence,
                created_at_milliseconds,
                lifecycle
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)",
            params![
                &current.session_id.as_bytes()[..],
                &current.workspace_id[..],
                current.display_name.as_deref(),
                sequence_to_sql(current.accepted_sequence)?,
                sequence_to_sql(session_fact_sequence)?,
                time_to_sql(current.accepted_at_milliseconds)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO session_run_states (
                session_id,
                active_run_id,
                entry_high_water,
                updated_sequence
             ) VALUES (?1, NULL, 0, ?2)",
            params![
                &current.session_id.as_bytes()[..],
                sequence_to_sql(session_fact_sequence)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO delivery_events (
                event_id,
                event_sequence,
                session_id,
                event_kind,
                payload_version,
                created_at_milliseconds
            ) VALUES (?1, ?2, ?3, 1, 1, ?4)",
            params![
                &delivery_event_id[..],
                sequence_to_sql(session_fact_sequence)?,
                &current.session_id.as_bytes()[..],
                time_to_sql(current.accepted_at_milliseconds)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO audit_facts (
                audit_id,
                audit_sequence,
                request_id,
                session_id,
                audit_kind,
                created_at_milliseconds
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                &audit_id[..],
                sequence_to_sql(audit_sequence)?,
                &current.request_id.as_bytes()[..],
                &current.session_id.as_bytes()[..],
                AUDIT_KIND_SESSION_CREATE_COMPLETED,
                time_to_sql(completed_at_milliseconds)?,
            ],
        )?;
        let updated = transaction.execute(
            "UPDATE session_creation_requests SET state = ?2 WHERE request_id = ?1 AND state = ?3",
            params![
                &current.request_id.as_bytes()[..],
                CREATION_STATE_READY,
                CREATION_STATE_WORKSPACE_DISPATCHED,
            ],
        )?;
        if updated != 1 {
            return Err(PersistenceError::InvalidState {
                reason: "a session creation request did not reach its terminal state",
            });
        }
        transaction.commit()?;
        self.load_required_session(current.session_id)
    }
}
