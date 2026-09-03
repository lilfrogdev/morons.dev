use std::path::Path;

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use sha2::{Digest as _, Sha256};

use super::{
    Backend,
    records::{
        current_time_milliseconds, load_mutation_operation, load_session, next_sequence,
        random_identifier, sequence_to_sql, time_to_sql,
    },
    run_acceptance::insert_delivery_event,
};
use crate::{
    persistence::{
        AcceptedLocalCommand, LocalCommandCancellationResult, LocalCommandId, LocalCommandStatus,
        MessageId, MutationRequestId, PersistenceError, PersistenceResourceLimit, SessionId,
        TranscriptEntry,
        run_types::{MAX_TRANSCRIPT_ENTRIES, MAX_TRANSCRIPT_TEXT_BYTES},
        types::REQUEST_FINGERPRINT_BYTES,
    },
    tools::{
        MAX_BASH_COMMAND_BYTES, MAX_TOOL_PAYLOAD_BYTES, ToolErrorKind, ToolKind, ToolOutput,
        ToolResult, validate_canonical_result,
    },
};

pub(super) const MUTATION_OPERATION_LOCAL_COMMAND: i64 = 10;
pub(super) const MUTATION_OPERATION_LOCAL_COMMAND_CANCEL: i64 = 11;
pub(super) const EVENT_LOCAL_COMMAND_CHANGED: i64 = 16;
pub(super) const EVENT_LOCAL_COMMAND_ENTRY: i64 = 17;
const STATE_ACCEPTED: i64 = 1;
const STATE_ACTIVE: i64 = 2;
const STATE_COMPLETED: i64 = 3;
const STATE_INTERRUPTED: i64 = 4;
const STATE_UNCERTAIN: i64 = 5;
const AUDIT_ACCEPTED: i64 = 1;
const AUDIT_ACTIVE: i64 = 2;
const AUDIT_COMPLETED: i64 = 3;
const AUDIT_CANCELLATION_REQUESTED: i64 = 4;
const FINGERPRINT_CONTEXT: &[u8] = b"morons.dev/local-command/v1\0";
const CANCEL_FINGERPRINT_CONTEXT: &[u8] = b"morons.dev/cancel-local-command/v1\0";

impl Backend {
    pub(crate) fn find_local_command_retry(
        &self,
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        session_id: SessionId,
        command: &str,
        context_visible: bool,
    ) -> Result<Option<AcceptedLocalCommand>, PersistenceError> {
        let existing = self.load_local_command_by_request(request_id)?;
        match (
            existing,
            load_mutation_operation(&self.connection, request_id)?,
        ) {
            (Some(existing), Some(MUTATION_OPERATION_LOCAL_COMMAND))
                if existing.session_id == session_id
                    && existing.command == command
                    && existing.context_visible == context_visible
                    && fingerprint
                        == local_command_fingerprint(session_id, command, context_visible) =>
            {
                Ok(Some(existing))
            }
            (Some(_), _) | (None, Some(_)) => Err(PersistenceError::RequestConflict),
            (None, None) => Ok(None),
        }
    }

    pub(crate) fn accept_local_command(
        &mut self,
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        session_id: SessionId,
        command: String,
        context_visible: bool,
    ) -> Result<AcceptedLocalCommand, PersistenceError> {
        if let Some(existing) = self.find_local_command_retry(
            request_id,
            fingerprint,
            session_id,
            &command,
            context_visible,
        )? {
            return Ok(existing);
        }
        let session =
            load_session(&self.connection, session_id)?.ok_or(PersistenceError::SessionNotFound)?;
        let archive_pending: bool = self.connection.query_row(
            "SELECT EXISTS (
                SELECT 1 FROM session_archive_requests
                WHERE session_id = ?1 AND archived = 1 AND state = 1
             )",
            [&session_id.as_bytes()[..]],
            |row| row.get(0),
        )?;
        if session.archived || archive_pending {
            return Err(PersistenceError::SessionArchived);
        }
        let working_directory = session
            .working_directory
            .as_deref()
            .filter(|path| Path::new(path).is_dir())
            .ok_or(PersistenceError::WorkingDirectoryUnavailable)?;
        let _ = working_directory;
        let active_run = self.connection.query_row(
            "SELECT active_run_id FROM session_run_states WHERE session_id = ?1",
            [&session_id.as_bytes()[..]],
            |row| row.get::<_, Option<[u8; 16]>>(0),
        )?;
        if let Some(active_run) = active_run {
            return Err(PersistenceError::SessionBusy {
                active_run_id: crate::persistence::RunId::from_bytes(active_run),
            });
        }
        if let Some(active_command_id) = self.active_local_command(session_id)? {
            return Err(PersistenceError::SessionCommandBusy { active_command_id });
        }
        if fingerprint != local_command_fingerprint(session_id, &command, context_visible) {
            return Err(PersistenceError::InvalidInput {
                reason: "a local command fingerprint is invalid",
            });
        }
        let command_id = LocalCommandId::from_bytes(random_identifier()?);
        let accepted_event_id = random_identifier()?;
        let audit_id = random_identifier()?;
        let now = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sequence = next_sequence(&transaction)?;
        let audit_sequence = next_sequence(&transaction)?;
        transaction.execute(
            "INSERT INTO mutation_requests (
                request_id, operation_kind, accepted_sequence, accepted_at_milliseconds
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                &request_id.as_bytes()[..],
                MUTATION_OPERATION_LOCAL_COMMAND,
                sequence_to_sql(sequence)?,
                time_to_sql(now)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO local_commands (
                command_id, request_id, operation_fingerprint, session_id, command_text,
                context_visible, state, cancellation_requested, result_payload,
                entry_sequence, message_id, accepted_sequence, updated_sequence,
                accepted_at_milliseconds, updated_at_milliseconds, accepted_event_id,
                delivery_event_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 0, NULL, NULL, NULL, ?7, ?7, ?8, ?8, ?9, NULL)",
            params![
                &command_id.as_bytes()[..],
                &request_id.as_bytes()[..],
                &fingerprint[..],
                &session_id.as_bytes()[..],
                &command,
                context_visible,
                sequence_to_sql(sequence)?,
                time_to_sql(now)?,
                &accepted_event_id[..],
            ],
        )?;
        transaction.execute(
            "UPDATE sessions SET updated_sequence = ?1 WHERE session_id = ?2",
            params![sequence_to_sql(sequence)?, &session_id.as_bytes()[..]],
        )?;
        transaction.execute(
            "UPDATE session_run_states SET updated_sequence = ?1 WHERE session_id = ?2",
            params![sequence_to_sql(sequence)?, &session_id.as_bytes()[..]],
        )?;
        insert_delivery_event(
            &transaction,
            &accepted_event_id,
            sequence,
            session_id,
            EVENT_LOCAL_COMMAND_CHANGED,
            now,
        )?;
        insert_audit(
            &transaction,
            AuditFact {
                id: &audit_id,
                sequence: audit_sequence,
                command_id,
                request_id: Some(request_id),
                session_id,
                kind: AUDIT_ACCEPTED,
                now,
            },
        )?;
        transaction.commit()?;
        Ok(AcceptedLocalCommand {
            id: command_id,
            session_id,
            command,
            context_visible,
            newly_accepted: true,
        })
    }

    pub(crate) fn activate_local_command(
        &mut self,
        command_id: LocalCommandId,
    ) -> Result<bool, PersistenceError> {
        let Some((session_id, state, cancellation)) = self.load_local_command_state(command_id)?
        else {
            return Err(PersistenceError::InvalidState {
                reason: "an accepted local command is missing",
            });
        };
        if state >= STATE_COMPLETED || cancellation {
            return Ok(false);
        }
        if state != STATE_ACCEPTED {
            return Err(PersistenceError::InvalidState {
                reason: "a local command has an invalid activation state",
            });
        }
        let audit_id = random_identifier()?;
        let now = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sequence = next_sequence(&transaction)?;
        let audit_sequence = next_sequence(&transaction)?;
        transaction.execute(
            "UPDATE local_commands SET state = 2, updated_sequence = ?1,
                 updated_at_milliseconds = ?2 WHERE command_id = ?3 AND state = 1",
            params![
                sequence_to_sql(sequence)?,
                time_to_sql(now)?,
                &command_id.as_bytes()[..]
            ],
        )?;
        insert_audit(
            &transaction,
            AuditFact {
                id: &audit_id,
                sequence: audit_sequence,
                command_id,
                request_id: None,
                session_id,
                kind: AUDIT_ACTIVE,
                now,
            },
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub(crate) fn complete_local_command(
        &mut self,
        command_id: LocalCommandId,
        result: ToolResult,
    ) -> Result<TranscriptEntry, PersistenceError> {
        if !validate_canonical_result(ToolKind::Bash, &result) {
            return Err(PersistenceError::InvalidInput {
                reason: "a local command result is invalid",
            });
        }
        let (session_id, state, _) =
            self.load_local_command_state(command_id)?
                .ok_or(PersistenceError::InvalidState {
                    reason: "a local command completion is missing its accepted command",
                })?;
        if !matches!(state, STATE_ACCEPTED | STATE_ACTIVE) {
            return Err(PersistenceError::InvalidState {
                reason: "a local command is already terminal",
            });
        }
        let terminal_state = match result.error_kind() {
            Some(ToolErrorKind::Uncertain) => STATE_UNCERTAIN,
            Some(
                ToolErrorKind::NotDispatched
                | ToolErrorKind::Interrupted
                | ToolErrorKind::Cancelled
                | ToolErrorKind::OutputLimit
                | ToolErrorKind::TimedOut
                | ToolErrorKind::InactivityTimeout,
            ) => STATE_INTERRUPTED,
            Some(_) | None => STATE_COMPLETED,
        };
        let payload = serde_json::to_vec(&result).map_err(|_| PersistenceError::InvalidInput {
            reason: "a local command result could not be encoded",
        })?;
        if payload.len() > MAX_TOOL_PAYLOAD_BYTES {
            return Err(limit());
        }
        let message_id = MessageId::from_bytes(random_identifier()?);
        let event_id = random_identifier()?;
        let audit_id = random_identifier()?;
        let now = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let entry_high_water: i64 = transaction.query_row(
            "SELECT entry_high_water FROM session_run_states WHERE session_id = ?1",
            [&session_id.as_bytes()[..]],
            |row| row.get(0),
        )?;
        let entry_sequence = u64::try_from(entry_high_water)
            .ok()
            .and_then(|value| value.checked_add(1))
            .filter(|value| *value <= MAX_TRANSCRIPT_ENTRIES)
            .ok_or_else(limit)?;
        let sequence = next_sequence(&transaction)?;
        let audit_sequence = next_sequence(&transaction)?;
        transaction.execute(
            "UPDATE local_commands SET state = ?1, result_payload = ?2,
                 entry_sequence = ?3, message_id = ?4, updated_sequence = ?5,
                 updated_at_milliseconds = ?6, delivery_event_id = ?7
             WHERE command_id = ?8 AND state IN (1, 2)",
            params![
                terminal_state,
                &payload,
                sequence_to_sql(entry_sequence)?,
                &message_id.as_bytes()[..],
                sequence_to_sql(sequence)?,
                time_to_sql(now)?,
                &event_id[..],
                &command_id.as_bytes()[..],
            ],
        )?;
        transaction.execute(
            "UPDATE session_run_states SET entry_high_water = ?1, updated_sequence = ?2
             WHERE session_id = ?3",
            params![
                sequence_to_sql(entry_sequence)?,
                sequence_to_sql(sequence)?,
                &session_id.as_bytes()[..]
            ],
        )?;
        transaction.execute(
            "UPDATE sessions SET updated_sequence = ?1 WHERE session_id = ?2",
            params![sequence_to_sql(sequence)?, &session_id.as_bytes()[..]],
        )?;
        insert_delivery_event(
            &transaction,
            &event_id,
            sequence,
            session_id,
            EVENT_LOCAL_COMMAND_ENTRY,
            now,
        )?;
        insert_audit(
            &transaction,
            AuditFact {
                id: &audit_id,
                sequence: audit_sequence,
                command_id,
                request_id: None,
                session_id,
                kind: AUDIT_COMPLETED,
                now,
            },
        )?;
        transaction.commit()?;
        self.load_local_command_entry(command_id)
    }

    pub(crate) fn cancel_local_command(
        &mut self,
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        session_id: SessionId,
        command_id: LocalCommandId,
    ) -> Result<LocalCommandCancellationResult, PersistenceError> {
        if let Some((stored, stored_session, stored_command, applied)) = self
            .connection
            .query_row(
                "SELECT operation_fingerprint, session_id, command_id, intent_applied
                 FROM local_command_cancellations WHERE request_id = ?1",
                [&request_id.as_bytes()[..]],
                |row| {
                    Ok((
                        row.get::<_, [u8; 32]>(0)?,
                        row.get::<_, [u8; 16]>(1)?,
                        row.get::<_, [u8; 16]>(2)?,
                        row.get::<_, bool>(3)?,
                    ))
                },
            )
            .optional()?
        {
            if stored != fingerprint
                || stored_session != *session_id.as_bytes()
                || stored_command != *command_id.as_bytes()
            {
                return Err(PersistenceError::RequestConflict);
            }
            return Ok(LocalCommandCancellationResult {
                command_id,
                cancellation_requested: applied,
                intent_applied: false,
            });
        }
        if load_mutation_operation(&self.connection, request_id)?.is_some()
            || fingerprint != cancel_local_command_fingerprint(session_id, command_id)
        {
            return Err(PersistenceError::RequestConflict);
        }
        let (stored_session, state, already_requested) = self
            .load_local_command_state(command_id)?
            .ok_or(PersistenceError::LocalCommandNotFound)?;
        if stored_session != session_id {
            return Err(PersistenceError::LocalCommandNotFound);
        }
        let applied = state < STATE_COMPLETED && !already_requested;
        let audit_id = random_identifier()?;
        let now = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sequence = next_sequence(&transaction)?;
        let audit_sequence = next_sequence(&transaction)?;
        transaction.execute(
            "INSERT INTO mutation_requests (request_id, operation_kind, accepted_sequence, accepted_at_milliseconds)
             VALUES (?1, ?2, ?3, ?4)",
            params![&request_id.as_bytes()[..], MUTATION_OPERATION_LOCAL_COMMAND_CANCEL, sequence_to_sql(sequence)?, time_to_sql(now)?],
        )?;
        transaction.execute(
            "INSERT INTO local_command_cancellations (
                request_id, operation_fingerprint, session_id, command_id, intent_applied,
                accepted_sequence, accepted_at_milliseconds
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                &request_id.as_bytes()[..],
                &fingerprint[..],
                &session_id.as_bytes()[..],
                &command_id.as_bytes()[..],
                applied,
                sequence_to_sql(sequence)?,
                time_to_sql(now)?
            ],
        )?;
        if applied {
            transaction.execute(
                "UPDATE local_commands SET cancellation_requested = 1, updated_sequence = ?1,
                     updated_at_milliseconds = ?2 WHERE command_id = ?3",
                params![
                    sequence_to_sql(sequence)?,
                    time_to_sql(now)?,
                    &command_id.as_bytes()[..]
                ],
            )?;
        }
        insert_audit(
            &transaction,
            AuditFact {
                id: &audit_id,
                sequence: audit_sequence,
                command_id,
                request_id: Some(request_id),
                session_id,
                kind: AUDIT_CANCELLATION_REQUESTED,
                now,
            },
        )?;
        transaction.commit()?;
        Ok(LocalCommandCancellationResult {
            command_id,
            cancellation_requested: applied || already_requested,
            intent_applied: applied,
        })
    }

    pub(super) fn recover_local_commands(&mut self) -> Result<(), PersistenceError> {
        let commands = {
            let mut statement = self.connection.prepare(
                "SELECT command_id, state FROM local_commands WHERE state IN (1, 2) ORDER BY accepted_sequence",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        LocalCommandId::from_bytes(row.get(0)?),
                        row.get::<_, i64>(1)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (command_id, state) in commands {
            self.complete_local_command(
                command_id,
                ToolResult::error(if state == STATE_ACTIVE {
                    ToolErrorKind::Uncertain
                } else {
                    ToolErrorKind::NotDispatched
                }),
            )?;
        }
        Ok(())
    }

    pub(crate) fn active_local_command(
        &self,
        session_id: SessionId,
    ) -> Result<Option<LocalCommandId>, PersistenceError> {
        self.connection
            .query_row(
                "SELECT command_id FROM local_commands WHERE session_id = ?1 AND state IN (1, 2)",
                [&session_id.as_bytes()[..]],
                |row| row.get::<_, [u8; 16]>(0).map(LocalCommandId::from_bytes),
            )
            .optional()
            .map_err(PersistenceError::from)
    }

    pub(super) fn list_local_command_entries(
        &self,
        session_id: SessionId,
        after_entry_sequence: u64,
        snapshot_entry_sequence: u64,
        snapshot_event_sequence: u64,
        limit: u16,
    ) -> Result<Vec<TranscriptEntry>, PersistenceError> {
        let mut statement = self.connection.prepare(
            "SELECT entry_sequence, message_id, command_text, context_visible,
                    result_payload, updated_at_milliseconds, command_id
             FROM local_commands
             WHERE session_id = ?1 AND entry_sequence > ?2 AND entry_sequence <= ?3
               AND updated_sequence <= ?4 AND state BETWEEN 3 AND 5
             ORDER BY entry_sequence LIMIT ?5",
        )?;
        statement
            .query_map(
                params![
                    &session_id.as_bytes()[..],
                    sequence_to_sql(after_entry_sequence)?,
                    sequence_to_sql(snapshot_entry_sequence)?,
                    sequence_to_sql(snapshot_event_sequence)?,
                    i64::from(limit),
                ],
                |row| local_command_entry_from_row(row, LocalCommandId::from_bytes(row.get(6)?)),
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(PersistenceError::from)
    }

    pub(super) fn load_local_command_entry_for_event(
        &self,
        session_id: SessionId,
        event_id: &[u8; 16],
    ) -> Result<TranscriptEntry, PersistenceError> {
        self.connection
            .query_row(
                "SELECT entry_sequence, message_id, command_text, context_visible,
                        result_payload, updated_at_milliseconds, command_id
                 FROM local_commands WHERE session_id = ?1 AND delivery_event_id = ?2
                   AND state BETWEEN 3 AND 5",
                params![&session_id.as_bytes()[..], &event_id[..]],
                |row| local_command_entry_from_row(row, LocalCommandId::from_bytes(row.get(6)?)),
            )
            .optional()?
            .ok_or(PersistenceError::InvalidState {
                reason: "a local command event is missing its transcript entry",
            })
    }

    pub(super) fn load_local_command_entry(
        &self,
        command_id: LocalCommandId,
    ) -> Result<TranscriptEntry, PersistenceError> {
        self.connection
            .query_row(
                "SELECT entry_sequence, message_id, command_text, context_visible,
                        result_payload, updated_at_milliseconds
                 FROM local_commands WHERE command_id = ?1 AND state BETWEEN 3 AND 5",
                [&command_id.as_bytes()[..]],
                |row| local_command_entry_from_row(row, command_id),
            )
            .optional()?
            .ok_or(PersistenceError::InvalidState {
                reason: "a terminal local command is missing its transcript entry",
            })
    }

    fn load_local_command_by_request(
        &self,
        request_id: MutationRequestId,
    ) -> Result<Option<AcceptedLocalCommand>, PersistenceError> {
        self.connection
            .query_row(
                "SELECT command_id, session_id, command_text, context_visible
                 FROM local_commands WHERE request_id = ?1",
                [&request_id.as_bytes()[..]],
                |row| {
                    Ok(AcceptedLocalCommand {
                        id: LocalCommandId::from_bytes(row.get(0)?),
                        session_id: SessionId::from_bytes(row.get(1)?),
                        command: row.get(2)?,
                        context_visible: row.get(3)?,
                        newly_accepted: false,
                    })
                },
            )
            .optional()
            .map_err(PersistenceError::from)
    }

    fn load_local_command_state(
        &self,
        command_id: LocalCommandId,
    ) -> Result<Option<(SessionId, i64, bool)>, PersistenceError> {
        self.connection
            .query_row(
                "SELECT session_id, state, cancellation_requested FROM local_commands WHERE command_id = ?1",
                [&command_id.as_bytes()[..]],
                |row| Ok((SessionId::from_bytes(row.get(0)?), row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(PersistenceError::from)
    }
}

pub(super) fn local_command_entry_from_row(
    row: &rusqlite::Row<'_>,
    command_id: LocalCommandId,
) -> rusqlite::Result<TranscriptEntry> {
    let result: ToolResult = serde_json::from_slice(&row.get::<_, Vec<u8>>(4)?).map_err(|_| {
        rusqlite::Error::InvalidColumnType(
            4,
            "result_payload".to_owned(),
            rusqlite::types::Type::Blob,
        )
    })?;
    let (status, exit_code, signal, stdout, stderr) =
        command_result_fields(&result).ok_or_else(|| {
            rusqlite::Error::InvalidColumnType(
                4,
                "result_payload".to_owned(),
                rusqlite::types::Type::Blob,
            )
        })?;
    Ok(TranscriptEntry::LocalCommand {
        entry_sequence: u64::try_from(row.get::<_, i64>(0)?)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, -1))?,
        id: MessageId::from_bytes(row.get(1)?),
        command_id,
        command: row.get(2)?,
        context_visible: row.get(3)?,
        status,
        exit_code,
        signal,
        stdout,
        stderr,
        created_at_milliseconds: u64::try_from(row.get::<_, i64>(5)?)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(5, -1))?,
    })
}

type CommandResultFields = (LocalCommandStatus, Option<i32>, Option<u16>, String, String);

fn command_result_fields(result: &ToolResult) -> Option<CommandResultFields> {
    if !validate_canonical_result(ToolKind::Bash, result) {
        return None;
    }
    let status = match result {
        ToolResult::Ok { .. } => LocalCommandStatus::Succeeded,
        ToolResult::Error {
            error: ToolErrorKind::Uncertain,
            ..
        } => LocalCommandStatus::Uncertain,
        ToolResult::Error {
            error:
                ToolErrorKind::NotDispatched
                | ToolErrorKind::Interrupted
                | ToolErrorKind::Cancelled
                | ToolErrorKind::OutputLimit
                | ToolErrorKind::TimedOut
                | ToolErrorKind::InactivityTimeout,
            ..
        } => LocalCommandStatus::Interrupted,
        ToolResult::Error { .. } => LocalCommandStatus::Failed,
    };
    let output = match result {
        ToolResult::Ok { output }
        | ToolResult::Error {
            output: Some(output),
            ..
        } => Some(output),
        ToolResult::Error { output: None, .. } => None,
    };
    let (exit_code, signal, stdout, stderr) = match output {
        Some(ToolOutput::Bash {
            exit_code,
            signal,
            stdout,
            stderr,
        }) => (*exit_code, *signal, stdout.clone(), stderr.clone()),
        Some(_) => return None,
        None => (None, None, String::new(), String::new()),
    };
    Some((status, exit_code, signal, stdout, stderr))
}

struct AuditFact<'a> {
    id: &'a [u8; 16],
    sequence: u64,
    command_id: LocalCommandId,
    request_id: Option<MutationRequestId>,
    session_id: SessionId,
    kind: i64,
    now: u64,
}

fn insert_audit(
    transaction: &Transaction<'_>,
    audit: AuditFact<'_>,
) -> Result<(), PersistenceError> {
    transaction.execute(
        "INSERT INTO local_command_audit_facts (
            audit_id, audit_sequence, command_id, request_id, session_id, audit_kind,
            created_at_milliseconds
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            &audit.id[..],
            sequence_to_sql(audit.sequence)?,
            &audit.command_id.as_bytes()[..],
            audit.request_id.map(|id| *id.as_bytes()),
            &audit.session_id.as_bytes()[..],
            audit.kind,
            time_to_sql(audit.now)?
        ],
    )?;
    Ok(())
}

pub(in crate::persistence) fn local_command_fingerprint(
    session_id: SessionId,
    command: &str,
    context_visible: bool,
) -> [u8; REQUEST_FINGERPRINT_BYTES] {
    let mut digest = Sha256::new();
    digest.update(FINGERPRINT_CONTEXT);
    digest.update(session_id.as_bytes());
    digest.update((command.len() as u32).to_be_bytes());
    digest.update(command.as_bytes());
    digest.update([u8::from(context_visible)]);
    digest.finalize().into()
}

pub(in crate::persistence) fn cancel_local_command_fingerprint(
    session_id: SessionId,
    command_id: LocalCommandId,
) -> [u8; REQUEST_FINGERPRINT_BYTES] {
    let mut digest = Sha256::new();
    digest.update(CANCEL_FINGERPRINT_CONTEXT);
    digest.update(session_id.as_bytes());
    digest.update(command_id.as_bytes());
    digest.finalize().into()
}

pub(in crate::persistence) fn validate_local_command(
    command: &str,
) -> Result<(), PersistenceError> {
    if command.is_empty() || command.len() > MAX_BASH_COMMAND_BYTES || command.contains('\0') {
        return Err(PersistenceError::InvalidInput {
            reason: "a local command must be nonempty and within the command limit",
        });
    }
    if command.len() > MAX_TRANSCRIPT_TEXT_BYTES {
        return Err(limit());
    }
    Ok(())
}

const fn limit() -> PersistenceError {
    PersistenceError::ResourceLimit {
        resource: PersistenceResourceLimit::Transcript,
    }
}
