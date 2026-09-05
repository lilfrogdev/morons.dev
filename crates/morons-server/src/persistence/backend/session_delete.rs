use std::path::Path;

use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    Backend,
    records::{
        MUTATION_OPERATION_SESSION_DELETE, current_time_milliseconds, load_mutation_operation,
        next_sequence, random_identifier, sequence_to_sql, time_to_sql,
    },
};
use crate::persistence::{
    MutationRequestId, PersistenceError, SessionId, types::REQUEST_FINGERPRINT_BYTES,
};

const MAX_SESSION_DELETIONS: i64 = 100_000;
const MAX_DELETED_MUTATION_TOMBSTONES: i64 = 1_000_000;
const DELETE_PREPARED: i64 = 1;
const DELETE_DATABASE_CLEANED: i64 = 2;
const DELETE_COMPLETE: i64 = 3;

impl Backend {
    pub(crate) fn prepare_session_delete(
        &mut self,
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        session_id: SessionId,
    ) -> Result<bool, PersistenceError> {
        let operation = load_mutation_operation(&self.connection, request_id)?;
        let existing = self
            .connection
            .query_row(
                "SELECT operation_fingerprint, session_id, state
                 FROM session_delete_requests WHERE request_id = ?1",
                [&request_id.as_bytes()[..]],
                |row| {
                    Ok((
                        row.get::<_, [u8; 32]>(0)?,
                        row.get::<_, [u8; 16]>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        match (operation, existing) {
            (Some(MUTATION_OPERATION_SESSION_DELETE), Some(existing)) => {
                if existing.0 != fingerprint || existing.1 != *session_id.as_bytes() {
                    return Err(PersistenceError::RequestConflict);
                }
                return Ok(existing.2 == DELETE_COMPLETE);
            }
            (Some(_), _) | (None, Some(_)) => return Err(PersistenceError::RequestConflict),
            (None, None) => {}
        }

        let session = self
            .get_session(session_id)?
            .ok_or(PersistenceError::SessionNotFound)?;
        if !session.archived {
            return Err(PersistenceError::SessionNotArchived);
        }
        if let Some(selected) = session.working_directory.as_deref()
            && self
                .paths
                .attachment_session_directory_overlaps(session_id.as_bytes(), Path::new(selected))?
        {
            return Err(PersistenceError::InvalidInput {
                reason: "the selected directory overlaps Morons attachment state",
            });
        }
        let deletions: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM session_delete_requests",
            [],
            |row| row.get(0),
        )?;
        if deletions >= MAX_SESSION_DELETIONS {
            return Err(PersistenceError::ResourceLimit {
                resource: crate::persistence::PersistenceResourceLimit::Sessions,
            });
        }
        let retained_tombstones: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM deleted_mutation_tombstones",
            [],
            |row| row.get(0),
        )?;
        let session_mutations: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM (
                SELECT request_id FROM session_creation_requests WHERE session_id = ?1
                UNION SELECT request_id FROM run_input_requests WHERE session_id = ?1
                UNION SELECT request_id FROM run_cancellation_requests WHERE session_id = ?1
                UNION SELECT request_id FROM repository_import_requests WHERE session_id = ?1
                UNION SELECT request_id FROM tool_uncertainty_acknowledgements WHERE session_id = ?1
                UNION SELECT request_id FROM local_commands WHERE session_id = ?1
                UNION SELECT request_id FROM local_command_cancellations WHERE session_id = ?1
                UNION SELECT request_id FROM session_rename_requests WHERE session_id = ?1
                UNION SELECT request_id FROM session_archive_requests WHERE session_id = ?1
             )",
            [&session_id.as_bytes()[..]],
            |row| row.get(0),
        )?;
        if retained_tombstones
            .checked_add(session_mutations)
            .is_none_or(|count| count > MAX_DELETED_MUTATION_TOMBSTONES)
        {
            return Err(PersistenceError::ResourceLimit {
                resource: crate::persistence::PersistenceResourceLimit::Transcript,
            });
        }
        let now = current_time_milliseconds()?;
        let event_id = random_identifier()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_session_idle(&transaction, session_id)?;
        let sequence = next_sequence(&transaction)?;
        transaction.execute(
            "INSERT INTO mutation_requests (
                request_id, operation_kind, accepted_sequence, accepted_at_milliseconds
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                &request_id.as_bytes()[..],
                MUTATION_OPERATION_SESSION_DELETE,
                sequence_to_sql(sequence)?,
                time_to_sql(now)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO session_delete_requests (
                request_id, operation_fingerprint, session_id, state,
                accepted_sequence, accepted_at_milliseconds, delivery_event_id
             ) VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6)",
            params![
                &request_id.as_bytes()[..],
                &fingerprint[..],
                &session_id.as_bytes()[..],
                sequence_to_sql(sequence)?,
                time_to_sql(now)?,
                &event_id[..],
            ],
        )?;
        transaction.execute(
            "INSERT INTO session_delete_attachments (delete_request_id, attachment_id)
             SELECT ?1, attachment_id FROM image_attachments WHERE session_id = ?2
             UNION
             SELECT ?1, attachment_id FROM tool_image_attachments WHERE session_id = ?2",
            params![&request_id.as_bytes()[..], &session_id.as_bytes()[..]],
        )?;
        transaction.commit()?;
        Ok(false)
    }

    pub(crate) fn clean_session_database(
        &mut self,
        request_id: MutationRequestId,
    ) -> Result<(), PersistenceError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (session_bytes, state) = load_delete_state(&transaction, request_id)?;
        if state >= DELETE_DATABASE_CLEANED {
            transaction.rollback()?;
            return Ok(());
        }
        let session_id = SessionId::from_bytes(session_bytes);
        ensure_session_idle(&transaction, session_id)?;
        let archived: i64 = transaction
            .query_row(
                "SELECT archived FROM sessions WHERE session_id = ?1",
                [&session_bytes[..]],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(PersistenceError::SessionNotFound)?;
        if archived != 1 {
            return Err(PersistenceError::SessionNotArchived);
        }

        transaction.execute(
            "WITH session_mutations(request_id) AS (
                SELECT request_id FROM session_creation_requests WHERE session_id = ?2
                UNION SELECT request_id FROM run_input_requests WHERE session_id = ?2
                UNION SELECT request_id FROM run_cancellation_requests WHERE session_id = ?2
                UNION SELECT request_id FROM repository_import_requests WHERE session_id = ?2
                UNION SELECT request_id FROM tool_uncertainty_acknowledgements WHERE session_id = ?2
                UNION SELECT request_id FROM local_commands WHERE session_id = ?2
                UNION SELECT request_id FROM local_command_cancellations WHERE session_id = ?2
                UNION SELECT request_id FROM session_rename_requests WHERE session_id = ?2
                UNION SELECT request_id FROM session_archive_requests WHERE session_id = ?2
             )
             INSERT INTO deleted_mutation_tombstones (
                request_id, delete_request_id, operation_kind,
                accepted_sequence, accepted_at_milliseconds
             )
             SELECT mutation.request_id, ?1, mutation.operation_kind,
                    mutation.accepted_sequence, mutation.accepted_at_milliseconds
             FROM mutation_requests AS mutation
             JOIN session_mutations AS scoped ON scoped.request_id = mutation.request_id",
            params![&request_id.as_bytes()[..], &session_bytes[..]],
        )?;

        for statement in [
            "DELETE FROM sessions WHERE session_id = ?1",
            "DELETE FROM session_run_states WHERE session_id = ?1",
            "DELETE FROM runs WHERE session_id = ?1",
            "DELETE FROM tool_audit_facts WHERE session_id = ?1",
            "DELETE FROM tool_image_attachments WHERE session_id = ?1",
            "DELETE FROM tool_operation_facts WHERE session_id = ?1",
            "DELETE FROM tool_uncertainty_acknowledgements WHERE session_id = ?1",
            "DELETE FROM tool_calls WHERE session_id = ?1",
            "DELETE FROM image_attachments WHERE session_id = ?1",
            "DELETE FROM compaction_operations WHERE session_id = ?1",
            "DELETE FROM run_accepted_checkpoints WHERE run_id IN (SELECT run_id FROM run_accepted_facts WHERE session_id = ?1)",
            "DELETE FROM context_checkpoints WHERE session_id = ?1",
            "DELETE FROM provider_operation_facts WHERE run_id IN (SELECT run_id FROM run_accepted_facts WHERE session_id = ?1)",
            "DELETE FROM run_audit_facts WHERE session_id = ?1",
            "DELETE FROM run_cancellation_requests WHERE session_id = ?1",
            "DELETE FROM run_state_facts WHERE session_id = ?1",
            "DELETE FROM run_skill_snapshots WHERE run_id IN (SELECT run_id FROM run_accepted_facts WHERE session_id = ?1)",
            "DELETE FROM run_project_contexts WHERE run_id IN (SELECT run_id FROM run_accepted_facts WHERE session_id = ?1)",
            "DELETE FROM session_entries WHERE session_id = ?1",
            "DELETE FROM run_accepted_facts WHERE session_id = ?1",
            "DELETE FROM local_command_audit_facts WHERE session_id = ?1",
            "DELETE FROM local_command_cancellations WHERE session_id = ?1",
            "DELETE FROM local_commands WHERE session_id = ?1",
            "DELETE FROM active_worktree_generations WHERE session_id = ?1",
            "DELETE FROM workspace_generation_layouts WHERE session_id = ?1",
            "DELETE FROM worktree_generation_facts WHERE session_id = ?1",
            "DELETE FROM repository_import_audit_facts WHERE session_id = ?1",
            "DELETE FROM repository_import_facts WHERE session_id = ?1",
            "DELETE FROM repository_import_requests WHERE session_id = ?1",
            "DELETE FROM session_rename_requests WHERE session_id = ?1",
            "DELETE FROM session_archive_requests WHERE session_id = ?1",
            "DELETE FROM run_input_requests WHERE session_id = ?1",
            "DELETE FROM audit_facts WHERE session_id = ?1",
            "DELETE FROM workspace_operation_facts WHERE request_id IN (SELECT request_id FROM session_creation_requests WHERE session_id = ?1)",
            "DELETE FROM session_created_facts WHERE session_id = ?1",
            "DELETE FROM session_creation_requests WHERE session_id = ?1",
        ] {
            transaction.execute(statement, [&session_bytes[..]])?;
        }
        transaction.execute(
            "DELETE FROM mutation_requests
             WHERE request_id IN (
                SELECT request_id FROM deleted_mutation_tombstones
                WHERE delete_request_id = ?1
             )",
            [&request_id.as_bytes()[..]],
        )?;
        transaction.execute(
            "UPDATE session_delete_requests SET state = 2
             WHERE request_id = ?1 AND state = 1",
            [&request_id.as_bytes()[..]],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn complete_session_delete(
        &mut self,
        request_id: MutationRequestId,
    ) -> Result<SessionId, PersistenceError> {
        let (session_bytes, state, event_id, sequence, accepted_at) = self.connection.query_row(
            "SELECT session_id, state, delivery_event_id, accepted_sequence,
                    accepted_at_milliseconds
             FROM session_delete_requests WHERE request_id = ?1",
            [&request_id.as_bytes()[..]],
            |row| {
                Ok((
                    row.get::<_, [u8; 16]>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, [u8; 16]>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )?;
        let session_id = SessionId::from_bytes(session_bytes);
        if state == DELETE_COMPLETE {
            return Ok(session_id);
        }
        if state != DELETE_DATABASE_CLEANED {
            return Err(PersistenceError::InvalidState {
                reason: "session deletion files were cleaned before database cleanup",
            });
        }
        let attachment_ids = {
            let mut statement = self.connection.prepare(
                "SELECT attachment_id FROM session_delete_attachments
                 WHERE delete_request_id = ?1 ORDER BY attachment_id",
            )?;
            statement
                .query_map([&request_id.as_bytes()[..]], |row| {
                    row.get::<_, [u8; 16]>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for attachment_id in attachment_ids {
            self.paths
                .remove_attachment_file(&session_bytes, &attachment_id)?;
        }
        self.paths
            .remove_attachment_session_directory(&session_bytes)?;

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM delivery_events WHERE session_id = ?1",
            [&session_bytes[..]],
        )?;
        transaction.execute(
            "INSERT INTO delivery_events (
                event_id, event_sequence, session_id, event_kind,
                payload_version, created_at_milliseconds
             ) VALUES (?1, ?2, ?3, 20, 1, ?4)",
            params![&event_id[..], sequence, &session_bytes[..], accepted_at],
        )?;
        transaction.execute(
            "DELETE FROM session_delete_attachments WHERE delete_request_id = ?1",
            [&request_id.as_bytes()[..]],
        )?;
        transaction.execute(
            "UPDATE session_delete_requests SET state = 3
             WHERE request_id = ?1 AND state = 2",
            [&request_id.as_bytes()[..]],
        )?;
        transaction.commit()?;
        Ok(session_id)
    }

    pub(crate) fn recover_session_deletes(&mut self) -> Result<(), PersistenceError> {
        let requests = {
            let mut statement = self.connection.prepare(
                "SELECT request_id, state FROM session_delete_requests
                 WHERE state < 3 ORDER BY accepted_sequence",
            )?;
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, [u8; 16]>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (request_id, state) in requests {
            let request_id = MutationRequestId::from_bytes(request_id);
            if state == DELETE_PREPARED {
                self.clean_session_database(request_id)?;
            }
            self.complete_session_delete(request_id)?;
        }
        Ok(())
    }
}

fn load_delete_state(
    transaction: &rusqlite::Transaction<'_>,
    request_id: MutationRequestId,
) -> Result<([u8; 16], i64), PersistenceError> {
    transaction
        .query_row(
            "SELECT session_id, state FROM session_delete_requests WHERE request_id = ?1",
            [&request_id.as_bytes()[..]],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or(PersistenceError::InvalidState {
            reason: "session deletion has no prepared request",
        })
}

fn ensure_session_idle(
    transaction: &rusqlite::Transaction<'_>,
    session_id: SessionId,
) -> Result<(), PersistenceError> {
    let active_run = transaction
        .query_row(
            "SELECT active_run_id FROM session_run_states WHERE session_id = ?1",
            [&session_id.as_bytes()[..]],
            |row| row.get::<_, Option<[u8; 16]>>(0),
        )
        .optional()?
        .ok_or(PersistenceError::SessionNotFound)?;
    if let Some(active_run_id) = active_run {
        return Err(PersistenceError::SessionBusy {
            active_run_id: crate::persistence::RunId::from_bytes(active_run_id),
        });
    }
    if let Some(active_command_id) = transaction
        .query_row(
            "SELECT command_id FROM local_commands
             WHERE session_id = ?1 AND state IN (1, 2)",
            [&session_id.as_bytes()[..]],
            |row| row.get::<_, [u8; 16]>(0),
        )
        .optional()?
    {
        return Err(PersistenceError::SessionCommandBusy {
            active_command_id: crate::persistence::LocalCommandId::from_bytes(active_command_id),
        });
    }
    Ok(())
}
