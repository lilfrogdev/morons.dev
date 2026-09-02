use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use super::{
    Backend,
    records::{
        MUTATION_OPERATION_RUN_INPUT, current_time_milliseconds, load_mutation_operation,
        next_sequence, random_identifier, sequence_to_sql, time_to_sql,
    },
    run_records::{
        AUDIT_INPUT_ACCEPTED, EVENT_RUN_ACCEPTED, EVENT_USER_MESSAGE, RunInputRequest,
        accepted_run_from_request, load_run_input_request,
    },
};
use crate::{
    persistence::{
        AcceptedRun, MessageId, MutationRequestId, PersistenceError, PersistenceResourceLimit,
        RunId, RunModelSelection,
        run_types::{
            CONTEXT_POLICY_VERSION, MAX_CONTEXT_ENTRIES, MAX_TRANSCRIPT_ENTRIES,
            conservative_input_token_estimate,
        },
        types::{REQUEST_FINGERPRINT_BYTES, validate_model_selection, validate_user_text},
    },
    tools::{TOOL_CATALOG_VERSION, TOOL_LIMITS_VERSION},
};

const MAX_RUNS: i64 = 100_000;

impl Backend {
    pub(crate) fn find_run_input_retry(
        &self,
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
    ) -> Result<Option<AcceptedRun>, PersistenceError> {
        resolve_existing_input(&self.connection, request_id, &fingerprint)
    }

    pub(crate) fn accept_session_input(
        &mut self,
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        session_id: crate::persistence::SessionId,
        text: String,
        selection: RunModelSelection,
    ) -> Result<AcceptedRun, PersistenceError> {
        validate_user_text(&text)?;
        validate_model_selection(&selection)?;

        if let Some(existing) = resolve_existing_input(&self.connection, request_id, &fingerprint)?
        {
            return Ok(existing);
        }

        self.credentials.ensure_consistent()?;
        let credential = self.credentials.status();
        if !credential.configured {
            return Err(PersistenceError::CredentialNotConfigured);
        }

        let run_id = RunId::from_bytes(random_identifier()?);
        let user_message_id = MessageId::from_bytes(random_identifier()?);
        let input_fact_id = random_identifier()?;
        let run_fact_id = random_identifier()?;
        let input_delivery_event_id = random_identifier()?;
        let run_delivery_event_id = random_identifier()?;
        let audit_id = random_identifier()?;
        let accepted_at_milliseconds = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(existing) = resolve_existing_input(&transaction, request_id, &fingerprint)? {
            transaction.rollback()?;
            return Ok(existing);
        }
        if count_runs(&transaction)? >= MAX_RUNS {
            return Err(PersistenceError::ResourceLimit {
                resource: PersistenceResourceLimit::Runs,
            });
        }
        let (active_run_id, entry_high_water) = load_session_run_state(&transaction, session_id)?;
        if let Some(active_run_id) = active_run_id {
            return Err(PersistenceError::SessionBusy { active_run_id });
        }
        let workspace_state = transaction
            .query_row(
                "SELECT state FROM repository_import_requests
                 WHERE session_id = ?1 AND state IN (0, 1, 4)
                 ORDER BY accepted_sequence DESC LIMIT 1",
                [&session_id.as_bytes()[..]],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        match workspace_state {
            Some(0 | 1) => return Err(PersistenceError::WorkspaceBusy),
            Some(4) => return Err(PersistenceError::WorkspaceBlocked),
            Some(_) => {
                return Err(PersistenceError::InvalidState {
                    reason: "a session workspace has an invalid blocking state",
                });
            }
            None => {}
        }
        let uncertain_tool_effect: bool = transaction.query_row(
            "SELECT EXISTS (
                SELECT 1 FROM tool_operation_facts AS uncertain
                WHERE uncertain.session_id = ?1 AND uncertain.fact_kind = 6
                  AND NOT EXISTS (
                      SELECT 1 FROM tool_uncertainty_acknowledgements AS acknowledgement
                      WHERE acknowledgement.run_id = uncertain.run_id
                  )
             )",
            [&session_id.as_bytes()[..]],
            |row| row.get(0),
        )?;
        if uncertain_tool_effect {
            return Err(PersistenceError::WorkspaceBlocked);
        }
        let workspace_ready: bool = transaction.query_row(
            "SELECT EXISTS (
                SELECT 1 FROM repository_import_requests
                WHERE session_id = ?1 AND state = 2
             )",
            [&session_id.as_bytes()[..]],
            |row| row.get(0),
        )?;
        let execution_image_generation: Option<[u8; 16]> = None;
        let (tool_catalog_version, tool_limits_version) =
            if workspace_ready && selection.supports_tool_calls {
                (TOOL_CATALOG_VERSION, TOOL_LIMITS_VERSION)
            } else {
                (0, 0)
            };
        if entry_high_water
            .checked_add(2)
            .is_none_or(|reserved_high_water| reserved_high_water > MAX_TRANSCRIPT_ENTRIES)
        {
            return Err(PersistenceError::ResourceLimit {
                resource: PersistenceResourceLimit::Transcript,
            });
        }

        let estimated_input_tokens = estimate_context_tokens(
            &transaction,
            session_id,
            entry_high_water,
            text.len(),
            selection.maximum_input_tokens,
        )?;
        let source_entry_high_water = entry_high_water + 1;
        let input_fact_sequence = next_sequence(&transaction)?;
        let run_fact_sequence = next_sequence(&transaction)?;
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
                MUTATION_OPERATION_RUN_INPUT,
                sequence_to_sql(run_fact_sequence)?,
                time_to_sql(accepted_at_milliseconds)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO run_input_requests (
                request_id,
                operation_fingerprint,
                session_id,
                run_id,
                user_message_id,
                accepted_sequence,
                accepted_at_milliseconds
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                &request_id.as_bytes()[..],
                &fingerprint[..],
                &session_id.as_bytes()[..],
                &run_id.as_bytes()[..],
                &user_message_id.as_bytes()[..],
                sequence_to_sql(run_fact_sequence)?,
                time_to_sql(accepted_at_milliseconds)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO run_accepted_facts (
                fact_id,
                fact_sequence,
                request_id,
                session_id,
                run_id,
                user_message_id,
                open_code_service,
                model_id,
                protocol_revision,
                credential_generation,
                context_policy_version,
                source_entry_high_water,
                estimated_input_tokens,
                maximum_input_tokens,
                maximum_output_tokens,
                accepted_at_milliseconds,
                delivery_event_id,
                tool_catalog_version,
                tool_limits_version,
                execution_image_generation
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20
             )",
            params![
                &run_fact_id[..],
                sequence_to_sql(run_fact_sequence)?,
                &request_id.as_bytes()[..],
                &session_id.as_bytes()[..],
                &run_id.as_bytes()[..],
                &user_message_id.as_bytes()[..],
                selection.service.to_record(),
                &selection.model_id,
                i64::from(selection.protocol_revision),
                sequence_to_sql(credential.generation)?,
                i64::from(CONTEXT_POLICY_VERSION),
                sequence_to_sql(source_entry_high_water)?,
                i64::from(estimated_input_tokens),
                i64::from(selection.maximum_input_tokens),
                i64::from(selection.maximum_output_tokens),
                time_to_sql(accepted_at_milliseconds)?,
                &run_delivery_event_id[..],
                i64::from(tool_catalog_version),
                i64::from(tool_limits_version),
                execution_image_generation.as_ref().map(|id| &id[..]),
            ],
        )?;
        transaction.execute(
            "INSERT INTO session_entries (
                fact_id,
                fact_sequence,
                session_id,
                entry_sequence,
                message_id,
                run_id,
                entry_kind,
                actor_kind,
                open_code_service,
                model_id,
                text,
                refusal,
                created_at_milliseconds,
                delivery_event_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 1, NULL, NULL, ?7, 0, ?8, ?9)",
            params![
                &input_fact_id[..],
                sequence_to_sql(input_fact_sequence)?,
                &session_id.as_bytes()[..],
                sequence_to_sql(source_entry_high_water)?,
                &user_message_id.as_bytes()[..],
                &run_id.as_bytes()[..],
                &text,
                time_to_sql(accepted_at_milliseconds)?,
                &input_delivery_event_id[..],
            ],
        )?;
        transaction.execute(
            "INSERT INTO runs (
                run_id,
                session_id,
                user_message_id,
                open_code_service,
                model_id,
                protocol_revision,
                credential_generation,
                context_policy_version,
                tool_catalog_version,
                tool_limits_version,
                execution_image_generation,
                source_entry_high_water,
                estimated_input_tokens,
                maximum_input_tokens,
                maximum_output_tokens,
                provider_turns,
                tool_calls,
                tool_mutations,
                tool_result_bytes,
                state,
                cancellation_requested,
                failure_kind,
                accepted_sequence,
                updated_sequence,
                accepted_at_milliseconds,
                updated_at_milliseconds
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14, ?15, 0, 0, 0, 0, 1, 0, NULL, ?16, ?16, ?17, ?17
             )",
            params![
                &run_id.as_bytes()[..],
                &session_id.as_bytes()[..],
                &user_message_id.as_bytes()[..],
                selection.service.to_record(),
                &selection.model_id,
                i64::from(selection.protocol_revision),
                sequence_to_sql(credential.generation)?,
                i64::from(CONTEXT_POLICY_VERSION),
                i64::from(tool_catalog_version),
                i64::from(tool_limits_version),
                execution_image_generation.as_ref().map(|id| &id[..]),
                sequence_to_sql(source_entry_high_water)?,
                i64::from(estimated_input_tokens),
                i64::from(selection.maximum_input_tokens),
                i64::from(selection.maximum_output_tokens),
                sequence_to_sql(run_fact_sequence)?,
                time_to_sql(accepted_at_milliseconds)?,
            ],
        )?;
        transaction.execute(
            "UPDATE session_run_states
             SET active_run_id = ?1,
                 entry_high_water = ?2,
                 updated_sequence = ?3
             WHERE session_id = ?4 AND active_run_id IS NULL",
            params![
                &run_id.as_bytes()[..],
                sequence_to_sql(source_entry_high_water)?,
                sequence_to_sql(run_fact_sequence)?,
                &session_id.as_bytes()[..],
            ],
        )?;
        transaction.execute(
            "UPDATE sessions SET updated_sequence = ?1 WHERE session_id = ?2",
            params![
                sequence_to_sql(run_fact_sequence)?,
                &session_id.as_bytes()[..]
            ],
        )?;
        insert_delivery_event(
            &transaction,
            &input_delivery_event_id,
            input_fact_sequence,
            session_id,
            EVENT_USER_MESSAGE,
            accepted_at_milliseconds,
        )?;
        insert_delivery_event(
            &transaction,
            &run_delivery_event_id,
            run_fact_sequence,
            session_id,
            EVENT_RUN_ACCEPTED,
            accepted_at_milliseconds,
        )?;
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
                AUDIT_INPUT_ACCEPTED,
                time_to_sql(accepted_at_milliseconds)?,
            ],
        )?;
        transaction.commit()?;

        let request = RunInputRequest {
            fingerprint,
            session_id,
            run_id,
            user_message_id,
        };
        accepted_run_from_request(&self.connection, &request, true)
    }
}

fn resolve_existing_input(
    connection: &rusqlite::Connection,
    request_id: MutationRequestId,
    fingerprint: &[u8; REQUEST_FINGERPRINT_BYTES],
) -> Result<Option<AcceptedRun>, PersistenceError> {
    let request = load_run_input_request(connection, request_id)?;
    let operation = load_mutation_operation(connection, request_id)?;
    match (request, operation) {
        (Some(request), Some(MUTATION_OPERATION_RUN_INPUT)) => {
            if &request.fingerprint != fingerprint {
                return Err(PersistenceError::RequestConflict);
            }
            accepted_run_from_request(connection, &request, false).map(Some)
        }
        (Some(_), _) => Err(PersistenceError::InvalidState {
            reason: "a run input request is missing its mutation registry record",
        }),
        (None, Some(_)) => Err(PersistenceError::RequestConflict),
        (None, None) => Ok(None),
    }
}

fn load_session_run_state(
    transaction: &Transaction<'_>,
    session_id: crate::persistence::SessionId,
) -> Result<(Option<RunId>, u64), PersistenceError> {
    transaction
        .query_row(
            "SELECT active_run_id, entry_high_water
             FROM session_run_states
             WHERE session_id = ?1",
            [&session_id.as_bytes()[..]],
            |row| {
                let active = row.get::<_, Option<[u8; 16]>>(0)?.map(RunId::from_bytes);
                let high_water = row.get::<_, i64>(1)?;
                Ok((active, high_water))
            },
        )
        .optional()?
        .ok_or(PersistenceError::SessionNotFound)
        .and_then(|(active, high_water)| {
            u64::try_from(high_water)
                .map(|high_water| (active, high_water))
                .map_err(|_| PersistenceError::InvalidState {
                    reason: "a session entry high water is invalid",
                })
        })
}

fn estimate_context_tokens(
    transaction: &Transaction<'_>,
    session_id: crate::persistence::SessionId,
    entry_high_water: u64,
    new_text_bytes: usize,
    maximum_input_tokens: u32,
) -> Result<u32, PersistenceError> {
    let (entry_count, text_bytes) = transaction.query_row(
        "SELECT COUNT(*), COALESCE(SUM(
             COALESCE(length(CAST(entry.text AS BLOB)),
                      length(call.input_payload),
                      length(result.result_payload), 0)
         ), 0)
         FROM session_entries AS entry
         LEFT JOIN tool_calls AS call ON call.call_id = entry.tool_call_id
         LEFT JOIN tool_operation_facts AS result
           ON result.call_id = entry.tool_call_id AND result.fact_kind BETWEEN 3 AND 6
         WHERE entry.session_id = ?1 AND entry.entry_sequence <= ?2",
        params![
            &session_id.as_bytes()[..],
            sequence_to_sql(entry_high_water)?
        ],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;
    let entry_count = u64::try_from(entry_count).map_err(|_| PersistenceError::InvalidState {
        reason: "the session context entry count is invalid",
    })?;
    if entry_count >= MAX_CONTEXT_ENTRIES as u64 {
        return Err(PersistenceError::ResourceLimit {
            resource: PersistenceResourceLimit::Context,
        });
    }
    let text_bytes = u64::try_from(text_bytes).map_err(|_| PersistenceError::InvalidState {
        reason: "the session context byte count is invalid",
    })?;
    let total_entries = entry_count + 1;
    let total_text_bytes =
        text_bytes
            .checked_add(new_text_bytes as u64)
            .ok_or(PersistenceError::ResourceLimit {
                resource: PersistenceResourceLimit::Context,
            })?;
    let estimate = conservative_input_token_estimate(total_text_bytes, total_entries).ok_or(
        PersistenceError::ResourceLimit {
            resource: PersistenceResourceLimit::Context,
        },
    )?;
    if estimate == 0 || estimate > maximum_input_tokens {
        return Err(PersistenceError::ResourceLimit {
            resource: PersistenceResourceLimit::Context,
        });
    }
    Ok(estimate)
}

fn count_runs(transaction: &Transaction<'_>) -> Result<i64, PersistenceError> {
    Ok(
        transaction.query_row("SELECT COUNT(*) FROM run_accepted_facts", [], |row| {
            row.get(0)
        })?,
    )
}

pub(super) fn insert_delivery_event(
    transaction: &Transaction<'_>,
    event_id: &[u8; 16],
    sequence: u64,
    session_id: crate::persistence::SessionId,
    event_kind: i64,
    created_at_milliseconds: u64,
) -> Result<(), PersistenceError> {
    transaction.execute(
        "INSERT INTO delivery_events (
            event_id,
            event_sequence,
            session_id,
            event_kind,
            payload_version,
            created_at_milliseconds
         ) VALUES (?1, ?2, ?3, ?4, 1, ?5)",
        params![
            &event_id[..],
            sequence_to_sql(sequence)?,
            &session_id.as_bytes()[..],
            event_kind,
            time_to_sql(created_at_milliseconds)?,
        ],
    )?;
    Ok(())
}
