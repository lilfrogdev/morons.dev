use std::path::Path;

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use super::{
    Backend,
    image_attachment::AttachmentStaging,
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
        RunId, RunInputContext, RunModelSelection,
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
        context: RunInputContext,
    ) -> Result<AcceptedRun, PersistenceError> {
        let RunInputContext {
            skills,
            project,
            attachments,
        } = context;
        validate_user_text(&text)?;
        validate_model_selection(&selection)?;
        self.ensure_context_integrity()?;
        if let Some(existing) = resolve_existing_input(&self.connection, request_id, &fingerprint)?
        {
            return Ok(existing);
        }
        if !skills.is_valid()
            || !project.is_valid()
            || (!selection.supports_tool_calls && project != Default::default())
        {
            return Err(PersistenceError::InvalidInput {
                reason: "the accepted instruction context is invalid or incompatible",
            });
        }
        let project_bytes = project.context_bytes();
        if !crate::persistence::images::validate_prepared_attachments(&text, &attachments) {
            return Err(PersistenceError::InvalidInput {
                reason: "the accepted image attachments are invalid",
            });
        }

        let working_directory = self
            .connection
            .query_row(
                "SELECT working_directory FROM sessions WHERE session_id = ?1",
                [&session_id.as_bytes()[..]],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .ok_or(PersistenceError::SessionNotFound)?
            .ok_or(PersistenceError::WorkingDirectoryUnavailable)?;
        if !Path::new(&working_directory).is_dir() {
            return Err(PersistenceError::WorkingDirectoryUnavailable);
        }
        if !selection.supports_image_input
            && (!attachments.is_empty() || session_has_image_context(&self.connection, session_id)?)
        {
            return Err(PersistenceError::ImageInputUnsupported);
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
        let attachment_bytes = attachments
            .iter()
            .try_fold(0_u64, |bytes, attachment| {
                bytes.checked_add(attachment.bytes.len() as u64)
            })
            .ok_or(PersistenceError::ResourceLimit {
                resource: PersistenceResourceLimit::Context,
            })?;
        if (!crate::persistence::images::image_context_capacity_available(
            &self.connection,
            session_id,
            attachments.len(),
            attachment_bytes,
        )? && !has_uncompacted_history(&self.connection, session_id)?)
            || !crate::persistence::images::attachment_storage_available(
                &self.connection,
                attachment_bytes,
            )?
        {
            return Err(PersistenceError::ResourceLimit {
                resource: PersistenceResourceLimit::Context,
            });
        }
        let attachment_paths = self.paths.clone();
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
        let archived = transaction
            .query_row(
                "SELECT archived OR EXISTS (
                    SELECT 1 FROM session_archive_requests AS archive
                    WHERE archive.session_id = sessions.session_id
                      AND archive.archived = 1 AND archive.state = 1
                 ) FROM sessions WHERE session_id = ?1",
                [&session_id.as_bytes()[..]],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or(PersistenceError::SessionNotFound)?;
        if archived == 1 {
            return Err(PersistenceError::SessionArchived);
        }
        let (active_run_id, entry_high_water) = load_session_run_state(&transaction, session_id)?;
        if let Some(active_run_id) = active_run_id {
            return Err(PersistenceError::SessionBusy { active_run_id });
        }
        if let Some(active_command_id) = transaction
            .query_row(
                "SELECT command_id FROM local_commands WHERE session_id = ?1 AND state IN (1, 2)",
                [&session_id.as_bytes()[..]],
                |row| row.get::<_, [u8; 16]>(0),
            )
            .optional()?
        {
            return Err(PersistenceError::SessionCommandBusy {
                active_command_id: crate::persistence::LocalCommandId::from_bytes(
                    active_command_id,
                ),
            });
        }
        let execution_image_generation: Option<[u8; 16]> = None;
        let (tool_catalog_version, tool_limits_version) = if selection.supports_tool_calls {
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

        let (estimated_input_tokens, accepted_checkpoint_id) = estimate_context_tokens(
            &transaction,
            session_id,
            entry_high_water,
            NewContextSize {
                text_bytes: text.len(),
                instruction_bytes: skills
                    .context_bytes()
                    .ok_or(PersistenceError::ResourceLimit {
                        resource: PersistenceResourceLimit::Context,
                    })?
                    .saturating_add(project_bytes),
                instruction_items: skills.skills.len() + usize::from(project_bytes > 0),
                image_count: attachments.len(),
                maximum_input_tokens: selection.maximum_input_tokens,
            },
        )?;
        let staging = AttachmentStaging::stage(attachment_paths, session_id, &attachments)?;
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
        if let Some(checkpoint_id) = accepted_checkpoint_id {
            transaction.execute(
                "INSERT INTO run_accepted_checkpoints (run_id, checkpoint_id) VALUES (?1, ?2)",
                params![&run_id.as_bytes()[..], &checkpoint_id[..]],
            )?;
        }
        insert_run_skills(&transaction, run_id, &skills)?;
        if tool_catalog_version == 9 {
            super::project_context::insert(&transaction, run_id, &project)?;
        }
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
        insert_image_attachments(
            &transaction,
            session_id,
            run_id,
            user_message_id,
            accepted_at_milliseconds,
            staging.attachments(),
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
        staging.commit();

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

fn insert_image_attachments(
    transaction: &Transaction<'_>,
    session_id: crate::persistence::SessionId,
    run_id: RunId,
    user_message_id: MessageId,
    created_at_milliseconds: u64,
    attachments: &[crate::persistence::ImageAttachment],
) -> Result<(), PersistenceError> {
    for (index, attachment) in attachments.iter().enumerate() {
        transaction.execute(
            "INSERT INTO image_attachments (
                attachment_id, session_id, run_id, user_message_id, attachment_index,
                display_name, marker_start, media_type, width, height,
                byte_count, sha256, created_at_milliseconds
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                &attachment.id.as_bytes()[..],
                &session_id.as_bytes()[..],
                &run_id.as_bytes()[..],
                &user_message_id.as_bytes()[..],
                i64::try_from(index + 1).map_err(|_| PersistenceError::ResourceLimit {
                    resource: PersistenceResourceLimit::Context,
                })?,
                &attachment.display_name,
                i64::from(attachment.marker_start),
                i64::from(crate::persistence::images::media_type_record(
                    attachment.media_type
                )),
                i64::from(attachment.width),
                i64::from(attachment.height),
                i64::try_from(attachment.bytes).map_err(|_| {
                    PersistenceError::ResourceLimit {
                        resource: PersistenceResourceLimit::Context,
                    }
                })?,
                &attachment.digest[..],
                time_to_sql(created_at_milliseconds)?,
            ],
        )?;
    }
    Ok(())
}

fn insert_run_skills(
    transaction: &Transaction<'_>,
    run_id: RunId,
    skills: &crate::skills::RunSkillContext,
) -> Result<(), PersistenceError> {
    for (index, skill) in skills.skills.iter().enumerate() {
        transaction.execute(
            "INSERT INTO run_skill_snapshots (
                run_id, skill_index, skill_name, description, skill_file,
                skill_source, active, instructions
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                &run_id.as_bytes()[..],
                i64::try_from(index + 1).map_err(|_| PersistenceError::ResourceLimit {
                    resource: PersistenceResourceLimit::Context,
                })?,
                &skill.name,
                &skill.description,
                &skill.skill_file,
                skill.source.to_record(),
                skill.active,
                skill.instructions.as_deref(),
            ],
        )?;
    }
    Ok(())
}

struct NewContextSize {
    text_bytes: usize,
    instruction_bytes: usize,
    instruction_items: usize,
    image_count: usize,
    maximum_input_tokens: u32,
}

fn estimate_context_tokens(
    transaction: &Transaction<'_>,
    session_id: crate::persistence::SessionId,
    entry_high_water: u64,
    size: NewContextSize,
) -> Result<(u32, Option<[u8; 16]>), PersistenceError> {
    let checkpoint = transaction
        .query_row(
            "SELECT checkpoint_id, source_entry_high_water, length(CAST(summary AS BLOB))
             FROM context_checkpoints
             WHERE session_id = ?1 AND source_entry_high_water <= ?2
             ORDER BY source_entry_high_water DESC LIMIT 1",
            params![
                &session_id.as_bytes()[..],
                sequence_to_sql(entry_high_water)?
            ],
            |row| {
                Ok((
                    row.get::<_, [u8; 16]>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    let checkpoint_high_water = checkpoint.as_ref().map_or(0, |checkpoint| checkpoint.1);
    let checkpoint_bytes = checkpoint.as_ref().map_or(0, |checkpoint| checkpoint.2);
    let (entry_count, text_bytes) = transaction.query_row(
        "SELECT COUNT(*), COALESCE(SUM(bytes), 0) FROM (
             SELECT COALESCE(length(CAST(entry.text AS BLOB)),
                             length(call.input_payload),
                             length(result.result_payload), 0) AS bytes
             FROM session_entries AS entry
             LEFT JOIN tool_calls AS call ON call.call_id = entry.tool_call_id
             LEFT JOIN tool_operation_facts AS result
               ON result.call_id = entry.tool_call_id AND result.fact_kind BETWEEN 3 AND 6
             WHERE entry.session_id = ?1 AND entry.entry_sequence <= ?2
               AND entry.entry_sequence > ?3
             UNION ALL
             SELECT length(CAST(command.command_text AS BLOB)) + length(command.result_payload)
             FROM local_commands AS command
             WHERE command.session_id = ?1 AND command.entry_sequence <= ?2
               AND command.entry_sequence > ?3
               AND command.context_visible = 1 AND command.state BETWEEN 3 AND 5
         )",
        params![
            &session_id.as_bytes()[..],
            sequence_to_sql(entry_high_water)?,
            checkpoint_high_water,
        ],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;
    let entry_count = entry_count
        .checked_add(i64::from(checkpoint.is_some()))
        .ok_or(PersistenceError::ResourceLimit {
            resource: PersistenceResourceLimit::Context,
        })?;
    let entry_count = u64::try_from(entry_count).map_err(|_| PersistenceError::InvalidState {
        reason: "the session context entry count is invalid",
    })?;
    let can_compact = entry_count > u64::from(checkpoint.is_some());
    if !can_compact
        && entry_count
            .checked_add(1)
            .and_then(|entries| entries.checked_add(size.instruction_items as u64))
            .and_then(|entries| entries.checked_add(size.image_count as u64))
            .is_none_or(|entries| entries > MAX_CONTEXT_ENTRIES as u64)
    {
        return Err(PersistenceError::ResourceLimit {
            resource: PersistenceResourceLimit::Context,
        });
    }
    let text_bytes =
        text_bytes
            .checked_add(checkpoint_bytes)
            .ok_or(PersistenceError::ResourceLimit {
                resource: PersistenceResourceLimit::Context,
            })?;
    let text_bytes = u64::try_from(text_bytes).map_err(|_| PersistenceError::InvalidState {
        reason: "the session context byte count is invalid",
    })?;
    let total_entries = entry_count + 1 + size.instruction_items as u64 + size.image_count as u64;
    let total_text_bytes = text_bytes
        .checked_add(size.text_bytes as u64)
        .and_then(|bytes| bytes.checked_add(size.instruction_bytes as u64))
        .and_then(|bytes| bytes.checked_add((size.image_count as u64).checked_mul(8_192)?))
        .ok_or(PersistenceError::ResourceLimit {
            resource: PersistenceResourceLimit::Context,
        })?;
    let estimate = conservative_input_token_estimate(total_text_bytes, total_entries).ok_or(
        PersistenceError::ResourceLimit {
            resource: PersistenceResourceLimit::Context,
        },
    )?;
    let new_input_estimate = conservative_input_token_estimate(
        (size.text_bytes as u64)
            .saturating_add(size.instruction_bytes as u64)
            .saturating_add((size.image_count as u64).saturating_mul(8_192)),
        1 + size.instruction_items as u64 + size.image_count as u64,
    )
    .unwrap_or(u32::MAX);
    if estimate == 0
        || (!can_compact && estimate > size.maximum_input_tokens)
        || u64::from(new_input_estimate)
            .saturating_add(super::context_budget::CONTEXT_REQUEST_RESERVE)
            > u64::from(size.maximum_input_tokens)
    {
        return Err(PersistenceError::ResourceLimit {
            resource: PersistenceResourceLimit::Context,
        });
    }
    // Full-context admission is for pre-dispatch compaction, not permission to
    // send an oversized provider request. Dispatch recomputes its actual budget.
    Ok((
        estimate.min(size.maximum_input_tokens),
        checkpoint.map(|checkpoint| checkpoint.0),
    ))
}

fn has_uncompacted_history(
    connection: &rusqlite::Connection,
    session_id: crate::persistence::SessionId,
) -> Result<bool, PersistenceError> {
    Ok(connection.query_row(
        "WITH checkpoint(high_water) AS (SELECT COALESCE(MAX(source_entry_high_water), 0) FROM context_checkpoints WHERE session_id = ?1)
         SELECT EXISTS (
            SELECT 1 FROM session_entries WHERE session_id = ?1 AND entry_sequence > (SELECT high_water FROM checkpoint)
            UNION ALL
            SELECT 1 FROM local_commands WHERE session_id = ?1 AND entry_sequence > (SELECT high_water FROM checkpoint) AND state BETWEEN 3 AND 5 AND context_visible = 1
         )", [&session_id.as_bytes()[..]], |row| row.get(0),
    )?)
}

fn session_has_image_context(
    connection: &rusqlite::Connection,
    session_id: crate::persistence::SessionId,
) -> Result<bool, PersistenceError> {
    Ok(connection.query_row(
        "WITH checkpoint(high_water) AS (
            SELECT COALESCE(MAX(source_entry_high_water), 0)
            FROM context_checkpoints WHERE session_id = ?1
         )
         SELECT EXISTS (
            SELECT 1 FROM image_attachments AS attachment
            JOIN session_entries AS entry ON entry.message_id = attachment.user_message_id
            WHERE attachment.session_id = ?1
              AND entry.entry_sequence > (SELECT high_water FROM checkpoint)
            UNION ALL
            SELECT 1 FROM tool_image_attachments AS attachment
            JOIN session_entries AS entry ON entry.tool_call_id = attachment.call_id
            WHERE attachment.session_id = ?1 AND entry.entry_kind = 4
              AND entry.entry_sequence > (SELECT high_water FROM checkpoint)
        )",
        [&session_id.as_bytes()[..]],
        |row| row.get(0),
    )?)
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
