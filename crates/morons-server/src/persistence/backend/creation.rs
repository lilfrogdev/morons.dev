use rusqlite::{Transaction, TransactionBehavior, params};

use super::{
    Backend,
    records::{
        CREATION_STATE_PREPARED, CREATION_STATE_READY, CreationRequest,
        MUTATION_OPERATION_SESSION_CREATE, creation_request_from_row, current_time_milliseconds,
        load_creation_request, load_mutation_operation, next_sequence, random_identifier,
        sequence_to_sql, time_to_sql, validate_request_retry,
    },
};
use crate::persistence::{
    MutationRequestId, PersistenceError, PersistenceResourceLimit, Session, SessionId,
    types::{IDENTIFIER_BYTES, REQUEST_FINGERPRINT_BYTES},
};

const MAX_SESSIONS: i64 = 10_000;
const AUDIT_KIND_SESSION_CREATE_ACCEPTED: i64 = 1;

impl Backend {
    pub(crate) fn create_session(
        &mut self,
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        display_name: Option<String>,
    ) -> Result<Session, PersistenceError> {
        let mutation_operation = load_mutation_operation(&self.connection, request_id)?;
        let creation = match (
            load_creation_request(&self.connection, request_id)?,
            mutation_operation,
        ) {
            (Some(existing), Some(MUTATION_OPERATION_SESSION_CREATE)) => {
                validate_request_retry(&existing, &fingerprint, display_name.as_deref())?;
                existing
            }
            (Some(_), _) => {
                return Err(PersistenceError::InvalidState {
                    reason: "a session creation request is missing its mutation registry record",
                });
            }
            (None, Some(_)) => return Err(PersistenceError::RequestConflict),
            (None, None) => self.prepare_session_creation(request_id, fingerprint, display_name)?,
        };

        if creation.state == CREATION_STATE_READY {
            return self.load_required_session(creation.session_id);
        }

        let dispatched = self.dispatch_workspace_creation(&creation)?;
        self.paths
            .provision_workspace(&dispatched.workspace_id)
            .map_err(PersistenceError::from)?;
        self.finalize_session_creation(&dispatched)
    }

    fn prepare_session_creation(
        &mut self,
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        display_name: Option<String>,
    ) -> Result<CreationRequest, PersistenceError> {
        let session_id = SessionId::from_bytes(random_identifier()?);
        let workspace_id = random_identifier()?;
        let audit_id = random_identifier()?;
        let accepted_at_milliseconds = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        if count_session_creation_requests(&transaction)? >= MAX_SESSIONS {
            return Err(PersistenceError::ResourceLimit {
                resource: PersistenceResourceLimit::Sessions,
            });
        }
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
                MUTATION_OPERATION_SESSION_CREATE,
                sequence_to_sql(accepted_sequence)?,
                time_to_sql(accepted_at_milliseconds)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO session_creation_requests (
                request_id,
                operation_fingerprint,
                session_id,
                workspace_id,
                display_name,
                accepted_sequence,
                accepted_at_milliseconds,
                state
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                &request_id.as_bytes()[..],
                &fingerprint[..],
                &session_id.as_bytes()[..],
                &workspace_id[..],
                display_name.as_deref(),
                sequence_to_sql(accepted_sequence)?,
                time_to_sql(accepted_at_milliseconds)?,
                CREATION_STATE_PREPARED,
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
                &request_id.as_bytes()[..],
                &session_id.as_bytes()[..],
                AUDIT_KIND_SESSION_CREATE_ACCEPTED,
                time_to_sql(accepted_at_milliseconds)?,
            ],
        )?;
        transaction.commit()?;

        Ok(CreationRequest {
            request_id,
            fingerprint,
            session_id,
            workspace_id,
            display_name,
            accepted_sequence,
            accepted_at_milliseconds,
            state: CREATION_STATE_PREPARED,
        })
    }

    pub(super) fn recover_incomplete_session_creations(&mut self) -> Result<(), PersistenceError> {
        let requests = {
            let mut statement = self.connection.prepare(
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
                WHERE state != ?1
                ORDER BY accepted_sequence",
            )?;
            statement
                .query_map([CREATION_STATE_READY], creation_request_from_row)?
                .collect::<Result<Vec<_>, _>>()?
        };

        for request in requests {
            let dispatched = self.dispatch_workspace_creation(&request)?;
            self.paths
                .provision_workspace(&dispatched.workspace_id)
                .map_err(PersistenceError::from)?;
            self.finalize_session_creation(&dispatched)?;
        }
        Ok(())
    }

    pub(super) fn validate_ready_workspaces(&self) -> Result<(), PersistenceError> {
        let workspace_ids = {
            let mut statement = self
                .connection
                .prepare("SELECT workspace_id FROM sessions ORDER BY created_sequence")?;
            statement
                .query_map([], |row| row.get::<_, [u8; IDENTIFIER_BYTES]>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for workspace_id in workspace_ids {
            self.paths
                .validate_workspace(&workspace_id)
                .map_err(PersistenceError::from)?;
        }
        Ok(())
    }
}

fn count_session_creation_requests(transaction: &Transaction<'_>) -> Result<i64, PersistenceError> {
    Ok(transaction.query_row(
        "SELECT COUNT(*) FROM session_creation_requests",
        [],
        |row| row.get(0),
    )?)
}
