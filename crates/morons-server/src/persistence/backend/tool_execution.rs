use super::{
    Backend,
    records::{
        MUTATION_OPERATION_TOOL_UNCERTAINTY_ACKNOWLEDGEMENT, current_time_milliseconds,
        load_mutation_operation, next_sequence, random_identifier, sequence_to_sql, time_to_sql,
    },
    run_acceptance::insert_delivery_event,
    run_execution::facts::{
        ProviderCompletedFact, TransitionIdentifiers, append_run_transition,
        insert_provider_completed, insert_run_audit, load_entry_high_water,
        require_dispatched_active_operation,
    },
    run_records::{
        AUDIT_PROVIDER_COMPLETED, AUDIT_RUN_STOPPED, EVENT_ASSISTANT_MESSAGE, EVENT_TOOL_CALL,
        EVENT_TOOL_RESULT, EVENT_TOOL_UNCERTAINTY_CHANGED, load_required_run,
    },
};
use crate::{
    persistence::{
        CommittedToolCall, CommittedToolTurn, CompletedToolTurn, MessageId, MutationRequestId,
        PersistenceError, PersistenceResourceLimit, RunId, RunState, ToolCallId,
        ToolUncertaintyAcknowledgement, TranscriptEntry,
        run_types::{
            MAX_TRANSCRIPT_ENTRIES, MAX_TRANSCRIPT_TEXT_BYTES, ProviderOperationId, ToolOperationId,
        },
        types::{REQUEST_FINGERPRINT_BYTES, acknowledge_tool_uncertainty_fingerprint},
    },
    tools::{
        MAX_TOOL_CALLS_PER_RUN, MAX_TOOL_MUTATIONS_PER_RUN, MAX_TOOL_PAYLOAD_BYTES,
        MAX_TOOL_RESULT_BYTES_PER_RUN, ToolErrorKind, ToolInput, ToolKind, ToolResult,
        WorktreeToolExecutor, tool_path_digest,
    },
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

const TOOL_PAYLOAD_VERSION: i64 = 1;
const TOOL_TERMINAL_RESULT_RESERVE_BYTES: u64 = 16 * 1024;
const TOOL_FACT_PREPARED: i64 = 1;
const TOOL_FACT_DISPATCHED: i64 = 2;
const TOOL_FACT_COMPLETED: i64 = 3;
const TOOL_FACT_NOT_DISPATCHED: i64 = 4;
const TOOL_FACT_INTERRUPTED: i64 = 5;
const TOOL_FACT_UNCERTAIN: i64 = 6;
const TOOL_RESULT_SUCCEEDED: i64 = 1;
const TOOL_RESULT_FAILED: i64 = 2;
const TOOL_RESULT_INTERRUPTED: i64 = 3;
const TOOL_RESULT_UNCERTAIN: i64 = 4;
const TOOL_AUDIT_CALL_COMMITTED: i64 = 1;
const TOOL_AUDIT_PREPARED: i64 = 2;
const TOOL_AUDIT_DISPATCHED: i64 = 3;
const TOOL_AUDIT_COMPLETED: i64 = 4;
const TOOL_AUDIT_UNCERTAIN: i64 = 5;

impl Backend {
    pub(crate) fn complete_provider_tool_turn(
        &mut self,
        run_id: RunId,
        operation_id: ProviderOperationId,
        turn: CompletedToolTurn,
    ) -> Result<CommittedToolTurn, PersistenceError> {
        validate_tool_turn(&turn)?;
        let operation_fact_id = random_identifier()?;
        let operation_audit_id = random_identifier()?;
        let now = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = load_required_run(&transaction, run_id)?;
        require_dispatched_active_operation(&transaction, &run, operation_id)?;
        let expected_high_water = provider_source_high_water(&transaction, operation_id)?;
        let entry_high_water = load_entry_high_water(&transaction, run.session_id)?;
        if expected_high_water != entry_high_water {
            return Err(PersistenceError::InvalidState {
                reason: "a tool turn transcript changed before its provider outcome committed",
            });
        }
        for call in &turn.calls {
            let duplicate: bool = transaction.query_row(
                "SELECT EXISTS (
                    SELECT 1 FROM tool_calls
                    WHERE run_id = ?1 AND provider_call_id = ?2
                 )",
                params![&run.id.as_bytes()[..], &call.provider_call_id],
                |row| row.get(0),
            )?;
            if duplicate {
                return Err(PersistenceError::InvalidInput {
                    reason: "a provider tool call identifier was already used in this run",
                });
            }
        }
        let new_call_count = run
            .tool_calls
            .checked_add(u32::try_from(turn.calls.len()).map_err(|_| limit())?)
            .filter(|count| *count <= MAX_TOOL_CALLS_PER_RUN)
            .ok_or_else(limit)?;
        let mutations = turn
            .calls
            .iter()
            .filter(|call| call.input.kind().is_mutation())
            .count();
        let new_mutation_count = run
            .tool_mutations
            .checked_add(u32::try_from(mutations).map_err(|_| limit())?)
            .filter(|count| *count <= MAX_TOOL_MUTATIONS_PER_RUN)
            .ok_or_else(limit)?;
        let additional_entries = turn.calls.len() + usize::from(turn.commentary.is_some());
        let final_entry_high_water = entry_high_water
            .checked_add(u64::try_from(additional_entries).map_err(|_| transcript_limit())?)
            .filter(|high_water| *high_water <= MAX_TRANSCRIPT_ENTRIES)
            .ok_or_else(transcript_limit)?;

        let operation_sequence = next_sequence(&transaction)?;
        let operation_audit_sequence = next_sequence(&transaction)?;
        insert_provider_completed(
            &transaction,
            ProviderCompletedFact {
                fact_id: &operation_fact_id,
                fact_sequence: operation_sequence,
                operation_id,
                run_id,
                provider_response_id: &turn.provider_response_id,
                usage: turn.usage,
                created_at_milliseconds: now,
            },
        )?;
        insert_run_audit(
            &transaction,
            &operation_audit_id,
            operation_audit_sequence,
            &run,
            Some(operation_id),
            AUDIT_PROVIDER_COMPLETED,
            now,
        )?;

        let mut next_entry = entry_high_water;
        let mut latest_sequence = operation_sequence;
        if let Some((text, refusal)) = turn.commentary {
            next_entry = next_entry.checked_add(1).ok_or_else(transcript_limit)?;
            latest_sequence =
                insert_assistant_commentary(&transaction, &run, next_entry, text, refusal, now)?;
        }

        let mut committed = Vec::with_capacity(turn.calls.len());
        for (index, call) in turn.calls.into_iter().enumerate() {
            next_entry = next_entry.checked_add(1).ok_or_else(transcript_limit)?;
            let call_id = ToolCallId::from_bytes(random_identifier()?);
            let tool_operation_id = ToolOperationId::from_bytes(random_identifier()?);
            let call_fact_sequence = next_sequence(&transaction)?;
            let call_entry_fact_sequence = next_sequence(&transaction)?;
            let call_audit_sequence = next_sequence(&transaction)?;
            let call_entry_id = MessageId::from_bytes(random_identifier()?);
            let call_entry_fact_id = random_identifier()?;
            let delivery_event_id = random_identifier()?;
            let call_audit_id = random_identifier()?;
            let input_payload = encode_payload(&call.input)?;
            let path_digest = tool_path_digest(call.input.path().as_str());
            transaction.execute(
                "INSERT INTO tool_calls (
                    call_id, operation_id, provider_operation_id, provider_call_id,
                    session_id, run_id, call_index, tool_kind, input_version,
                    input_payload, path_digest, fact_sequence, created_at_milliseconds
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9, ?10, ?11, ?12)",
                params![
                    &call_id.as_bytes()[..],
                    &tool_operation_id.as_bytes()[..],
                    &operation_id.as_bytes()[..],
                    &call.provider_call_id,
                    &run.session_id.as_bytes()[..],
                    &run.id.as_bytes()[..],
                    i64::try_from(index + 1).map_err(|_| limit())?,
                    call.input.kind().to_record(),
                    &input_payload,
                    &path_digest[..],
                    sequence_to_sql(call_fact_sequence)?,
                    time_to_sql(now)?,
                ],
            )?;
            transaction.execute(
                "INSERT INTO session_entries (
                    fact_id, fact_sequence, session_id, entry_sequence, message_id,
                    run_id, entry_kind, actor_kind, open_code_service, model_id,
                    text, refusal, assistant_phase, tool_call_id,
                    created_at_milliseconds, delivery_event_id
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, 3, 2, ?7, ?8,
                    NULL, 0, NULL, ?9, ?10, ?11
                 )",
                params![
                    &call_entry_fact_id[..],
                    sequence_to_sql(call_entry_fact_sequence)?,
                    &run.session_id.as_bytes()[..],
                    sequence_to_sql(next_entry)?,
                    &call_entry_id.as_bytes()[..],
                    &run.id.as_bytes()[..],
                    run.service.to_record(),
                    &run.model_id,
                    &call_id.as_bytes()[..],
                    time_to_sql(now)?,
                    &delivery_event_id[..],
                ],
            )?;
            insert_delivery_event(
                &transaction,
                &delivery_event_id,
                call_entry_fact_sequence,
                run.session_id,
                EVENT_TOOL_CALL,
                now,
            )?;
            let call_identity = load_tool_call_identity(&transaction, call_id)?;
            insert_tool_audit(
                &transaction,
                &call_audit_id,
                call_audit_sequence,
                &call_identity,
                TOOL_AUDIT_CALL_COMMITTED,
                now,
            )?;
            latest_sequence = call_entry_fact_sequence;
            committed.push(CommittedToolCall {
                call_id,
                operation_id: tool_operation_id,
                input: call.input,
            });
        }
        if next_entry != final_entry_high_water {
            return Err(PersistenceError::InvalidState {
                reason: "a provider tool turn reserved the wrong transcript range",
            });
        }
        transaction.execute(
            "UPDATE runs
             SET provider_turns = provider_turns + 1,
                 tool_calls = ?1,
                 tool_mutations = ?2,
                 updated_sequence = ?3,
                 updated_at_milliseconds = ?4
             WHERE run_id = ?5",
            params![
                i64::from(new_call_count),
                i64::from(new_mutation_count),
                sequence_to_sql(latest_sequence)?,
                time_to_sql(now)?,
                &run.id.as_bytes()[..],
            ],
        )?;
        update_entry_high_water(&transaction, &run, final_entry_high_water, latest_sequence)?;
        transaction.commit()?;
        Ok(CommittedToolTurn { calls: committed })
    }

    pub(crate) fn prepare_tool_operation(
        &mut self,
        run_id: RunId,
        call_id: ToolCallId,
        operation_id: ToolOperationId,
        recovery_plan: Option<Vec<u8>>,
    ) -> Result<(), PersistenceError> {
        if recovery_plan
            .as_ref()
            .is_some_and(|plan| plan.len() > MAX_TOOL_PAYLOAD_BYTES)
        {
            return Err(limit());
        }
        let fact_id = random_identifier()?;
        let audit_id = random_identifier()?;
        let now = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = load_required_run(&transaction, run_id)?;
        require_active_tool_call(&transaction, &run, call_id, operation_id)?;
        require_prior_calls_terminal(&transaction, run_id, call_id)?;
        let existing: bool = transaction.query_row(
            "SELECT EXISTS (SELECT 1 FROM tool_operation_facts WHERE call_id = ?1)",
            [&call_id.as_bytes()[..]],
            |row| row.get(0),
        )?;
        if existing {
            return Err(PersistenceError::InvalidState {
                reason: "a tool operation was already prepared",
            });
        }
        let sequence = next_sequence(&transaction)?;
        let audit_sequence = next_sequence(&transaction)?;
        let call = load_tool_call_identity(&transaction, call_id)?;
        transaction.execute(
            "INSERT INTO tool_operation_facts (
                fact_id, fact_sequence, call_id, operation_id, session_id, run_id,
                fact_kind, recovery_version, recovery_payload, result_version,
                result_payload, result_status, created_at_milliseconds,
                workspace_delivery_event_id
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8,
                NULL, NULL, NULL, ?9, NULL
             )",
            params![
                &fact_id[..],
                sequence_to_sql(sequence)?,
                &call_id.as_bytes()[..],
                &operation_id.as_bytes()[..],
                &run.session_id.as_bytes()[..],
                &run.id.as_bytes()[..],
                recovery_plan.as_ref().map(|_| TOOL_PAYLOAD_VERSION),
                recovery_plan.as_deref(),
                time_to_sql(now)?,
            ],
        )?;
        insert_tool_audit(
            &transaction,
            &audit_id,
            audit_sequence,
            &call,
            TOOL_AUDIT_PREPARED,
            now,
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn mark_tool_dispatched(
        &mut self,
        run_id: RunId,
        call_id: ToolCallId,
        operation_id: ToolOperationId,
    ) -> Result<(), PersistenceError> {
        let fact_id = random_identifier()?;
        let audit_id = random_identifier()?;
        let now = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = load_required_run(&transaction, run_id)?;
        require_active_tool_call(&transaction, &run, call_id, operation_id)?;
        require_tool_fact(&transaction, call_id, TOOL_FACT_PREPARED)?;
        ensure_tool_not_terminal(&transaction, call_id)?;
        let sequence = next_sequence(&transaction)?;
        let audit_sequence = next_sequence(&transaction)?;
        transaction.execute(
            "INSERT INTO tool_operation_facts (
                fact_id, fact_sequence, call_id, operation_id, session_id, run_id,
                fact_kind, recovery_version, recovery_payload, result_version,
                result_payload, result_status, created_at_milliseconds,
                workspace_delivery_event_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 2, NULL, NULL, NULL, NULL, NULL, ?7, NULL)",
            params![
                &fact_id[..],
                sequence_to_sql(sequence)?,
                &call_id.as_bytes()[..],
                &operation_id.as_bytes()[..],
                &run.session_id.as_bytes()[..],
                &run.id.as_bytes()[..],
                time_to_sql(now)?,
            ],
        )?;
        let call = load_tool_call_identity(&transaction, call_id)?;
        insert_tool_audit(
            &transaction,
            &audit_id,
            audit_sequence,
            &call,
            TOOL_AUDIT_DISPATCHED,
            now,
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn complete_tool_result(
        &mut self,
        run_id: RunId,
        call_id: ToolCallId,
        operation_id: ToolOperationId,
        mut result: ToolResult,
    ) -> Result<TranscriptEntry, PersistenceError> {
        let mut result_payload = encode_payload(&result)?;
        let mut result_bytes = u64::try_from(result_payload.len()).map_err(|_| limit())?;
        let fact_id = random_identifier()?;
        let entry_fact_id = random_identifier()?;
        let entry_id = MessageId::from_bytes(random_identifier()?);
        let entry_event_id = random_identifier()?;
        let audit_id = random_identifier()?;
        let transition = TransitionIdentifiers::generate()?;
        let now = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = load_required_run(&transaction, run_id)?;
        require_active_tool_call(&transaction, &run, call_id, operation_id)?;
        require_tool_fact(&transaction, call_id, TOOL_FACT_PREPARED)?;
        ensure_tool_not_terminal(&transaction, call_id)?;
        let dispatched = tool_has_fact(&transaction, call_id, TOOL_FACT_DISPATCHED)?;
        let ordinary_result_limit = MAX_TOOL_RESULT_BYTES_PER_RUN
            .checked_sub(TOOL_TERMINAL_RESULT_RESERVE_BYTES)
            .ok_or_else(limit)?;
        if run
            .tool_result_bytes
            .checked_add(result_bytes)
            .is_none_or(|bytes| bytes > ordinary_result_limit)
        {
            result = ToolResult::error(ToolErrorKind::ResourceLimit);
            result_payload = encode_payload(&result)?;
            result_bytes = u64::try_from(result_payload.len()).map_err(|_| limit())?;
        }
        let (fact_kind, result_status) = classify_result(&result, dispatched)?;
        let workspace_event_id = result.is_uncertain().then(random_identifier).transpose()?;
        let new_result_bytes = run
            .tool_result_bytes
            .checked_add(result_bytes)
            .filter(|bytes| *bytes <= MAX_TOOL_RESULT_BYTES_PER_RUN)
            .ok_or_else(limit)?;
        let entry_high_water = load_entry_high_water(&transaction, run.session_id)?;
        let entry_sequence = entry_high_water
            .checked_add(1)
            .filter(|sequence| *sequence <= MAX_TRANSCRIPT_ENTRIES)
            .ok_or_else(transcript_limit)?;
        let fact_sequence = next_sequence(&transaction)?;
        let entry_fact_sequence = next_sequence(&transaction)?;
        let audit_sequence = next_sequence(&transaction)?;
        transaction.execute(
            "INSERT INTO tool_operation_facts (
                fact_id, fact_sequence, call_id, operation_id, session_id, run_id,
                fact_kind, recovery_version, recovery_payload, result_version,
                result_payload, result_status, created_at_milliseconds,
                workspace_delivery_event_id
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, 1, ?8, ?9, ?10, ?11
             )",
            params![
                &fact_id[..],
                sequence_to_sql(fact_sequence)?,
                &call_id.as_bytes()[..],
                &operation_id.as_bytes()[..],
                &run.session_id.as_bytes()[..],
                &run.id.as_bytes()[..],
                fact_kind,
                &result_payload,
                result_status,
                time_to_sql(now)?,
                workspace_event_id.as_ref().map(|id| &id[..]),
            ],
        )?;
        transaction.execute(
            "INSERT INTO session_entries (
                fact_id, fact_sequence, session_id, entry_sequence, message_id,
                run_id, entry_kind, actor_kind, open_code_service, model_id,
                text, refusal, assistant_phase, tool_call_id,
                created_at_milliseconds, delivery_event_id
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, 4, 3, NULL, NULL,
                NULL, 0, NULL, ?7, ?8, ?9
             )",
            params![
                &entry_fact_id[..],
                sequence_to_sql(entry_fact_sequence)?,
                &run.session_id.as_bytes()[..],
                sequence_to_sql(entry_sequence)?,
                &entry_id.as_bytes()[..],
                &run.id.as_bytes()[..],
                &call_id.as_bytes()[..],
                time_to_sql(now)?,
                &entry_event_id[..],
            ],
        )?;
        insert_delivery_event(
            &transaction,
            &entry_event_id,
            entry_fact_sequence,
            run.session_id,
            EVENT_TOOL_RESULT,
            now,
        )?;
        if let Some(workspace_event_id) = workspace_event_id {
            insert_delivery_event(
                &transaction,
                &workspace_event_id,
                fact_sequence,
                run.session_id,
                EVENT_TOOL_UNCERTAINTY_CHANGED,
                now,
            )?;
        }
        let call = load_tool_call_identity(&transaction, call_id)?;
        insert_tool_audit(
            &transaction,
            &audit_id,
            audit_sequence,
            &call,
            if result.is_uncertain() {
                TOOL_AUDIT_UNCERTAIN
            } else {
                TOOL_AUDIT_COMPLETED
            },
            now,
        )?;
        transaction.execute(
            "UPDATE runs
             SET tool_result_bytes = ?1,
                 updated_sequence = ?2,
                 updated_at_milliseconds = ?3
             WHERE run_id = ?4",
            params![
                sequence_to_sql(new_result_bytes)?,
                sequence_to_sql(entry_fact_sequence)?,
                time_to_sql(now)?,
                &run.id.as_bytes()[..],
            ],
        )?;
        update_entry_high_water(&transaction, &run, entry_sequence, entry_fact_sequence)?;
        if result.is_uncertain() {
            append_run_transition(
                &transaction,
                &run,
                RunState::Uncertain,
                None,
                transition,
                now,
                AUDIT_RUN_STOPPED,
            )?;
        }
        transaction.commit()?;
        Ok(TranscriptEntry::ToolResult {
            entry_sequence,
            id: entry_id,
            run_id,
            call_id,
            operation_id,
            tool: call.kind,
            result,
            created_at_milliseconds: now,
        })
    }

    pub(super) fn recover_tool_operations(&mut self) -> Result<(), PersistenceError> {
        let operations = self.incomplete_tool_operations()?;
        for operation in operations {
            if !operation.prepared {
                self.prepare_tool_operation(
                    operation.run_id,
                    operation.call_id,
                    operation.operation_id,
                    None,
                )?;
                self.complete_tool_result(
                    operation.run_id,
                    operation.call_id,
                    operation.operation_id,
                    ToolResult::error(ToolErrorKind::NotDispatched),
                )?;
                continue;
            }
            let result = if operation.input.kind().is_mutation() {
                let plan = operation.recovery_plan.as_deref().ok_or(
                    PersistenceError::InvalidState {
                        reason: "an incomplete mutating tool operation is missing its recovery plan",
                    },
                )?;
                let generation_id = self.active_worktree_generation(&operation.workspace_id)?;
                WorktreeToolExecutor::new(
                    self.paths
                        .worktree_generation_path(&operation.workspace_id, &generation_id),
                )
                .recover_mutation(plan)
            } else if operation.dispatched {
                ToolResult::error(ToolErrorKind::Interrupted)
            } else {
                ToolResult::error(ToolErrorKind::NotDispatched)
            };
            self.complete_tool_result(
                operation.run_id,
                operation.call_id,
                operation.operation_id,
                result,
            )?;
        }
        Ok(())
    }

    fn incomplete_tool_operations(
        &self,
    ) -> Result<Vec<crate::persistence::ToolOperationRecovery>, PersistenceError> {
        let mut statement = self.connection.prepare(
            "SELECT
                call.run_id,
                call.call_id,
                call.operation_id,
                call.input_payload,
                prepared.fact_id IS NOT NULL,
                EXISTS (SELECT 1 FROM tool_operation_facts AS dispatched
                        WHERE dispatched.call_id = call.call_id AND dispatched.fact_kind = 2),
                prepared.recovery_payload,
                session.workspace_id
             FROM tool_calls AS call
             JOIN session_created_facts AS session ON session.session_id = call.session_id
             LEFT JOIN tool_operation_facts AS prepared
               ON prepared.call_id = call.call_id AND prepared.fact_kind = 1
             WHERE NOT EXISTS (
                 SELECT 1 FROM tool_operation_facts AS terminal
                 WHERE terminal.call_id = call.call_id AND terminal.fact_kind BETWEEN 3 AND 6
             )
             ORDER BY call.fact_sequence",
        )?;
        statement
            .query_map([], |row| {
                let payload = row.get::<_, Vec<u8>>(3)?;
                let input = serde_json::from_slice::<ToolInput>(&payload).map_err(|_| {
                    rusqlite::Error::InvalidColumnType(
                        3,
                        "input_payload".to_owned(),
                        rusqlite::types::Type::Blob,
                    )
                })?;
                Ok(crate::persistence::ToolOperationRecovery {
                    run_id: RunId::from_bytes(row.get(0)?),
                    call_id: ToolCallId::from_bytes(row.get(1)?),
                    operation_id: ToolOperationId::from_bytes(row.get(2)?),
                    input,
                    prepared: row.get(4)?,
                    dispatched: row.get(5)?,
                    recovery_plan: row.get(6)?,
                    workspace_id: row.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(PersistenceError::from)
    }

    pub(crate) fn acknowledge_tool_uncertainty(
        &mut self,
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        session_id: crate::persistence::SessionId,
        run_id: RunId,
    ) -> Result<ToolUncertaintyAcknowledgement, PersistenceError> {
        let existing = self
            .connection
            .query_row(
                "SELECT operation_fingerprint, session_id, run_id
                 FROM tool_uncertainty_acknowledgements WHERE request_id = ?1",
                [&request_id.as_bytes()[..]],
                |row| {
                    Ok((
                        row.get::<_, [u8; 32]>(0)?,
                        row.get::<_, [u8; 16]>(1)?,
                        row.get::<_, [u8; 16]>(2)?,
                    ))
                },
            )
            .optional()?;
        match (
            existing,
            load_mutation_operation(&self.connection, request_id)?,
        ) {
            (Some((stored, stored_session, stored_run)), Some(kind))
                if kind == MUTATION_OPERATION_TOOL_UNCERTAINTY_ACKNOWLEDGEMENT =>
            {
                if stored != fingerprint
                    || stored_session != *session_id.as_bytes()
                    || stored_run != *run_id.as_bytes()
                {
                    return Err(PersistenceError::RequestConflict);
                }
                return Ok(ToolUncertaintyAcknowledgement {
                    session_id,
                    run_id,
                    workspace: self.workspace_summary(session_id)?,
                });
            }
            (Some(_), _) => {
                return Err(PersistenceError::InvalidState {
                    reason: "a tool uncertainty acknowledgement lost its mutation record",
                });
            }
            (None, Some(_)) => return Err(PersistenceError::RequestConflict),
            (None, None) => {}
        }
        if fingerprint != acknowledge_tool_uncertainty_fingerprint(session_id, run_id) {
            return Err(PersistenceError::InvalidInput {
                reason: "a tool uncertainty acknowledgement fingerprint is invalid",
            });
        }
        let run = load_required_run(&self.connection, run_id)?;
        if run.session_id != session_id {
            return Err(PersistenceError::ToolUncertaintyNotFound);
        }
        if run.state != RunState::Uncertain {
            return Err(PersistenceError::ToolUncertaintyNotFound);
        }
        let uncertainty_exists: bool = self.connection.query_row(
            "SELECT EXISTS (
                SELECT 1 FROM tool_operation_facts
                WHERE session_id = ?1 AND run_id = ?2 AND fact_kind = 6
             )",
            params![&session_id.as_bytes()[..], &run_id.as_bytes()[..]],
            |row| row.get(0),
        )?;
        if !uncertainty_exists {
            return Err(PersistenceError::InvalidState {
                reason: "an uncertain run is missing its uncertain tool fact",
            });
        }
        let event_id = random_identifier()?;
        let audit_id = random_identifier()?;
        let now = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let fact_sequence = next_sequence(&transaction)?;
        let audit_sequence = next_sequence(&transaction)?;
        transaction.execute(
            "INSERT INTO mutation_requests (
                request_id, operation_kind, accepted_sequence, accepted_at_milliseconds
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                &request_id.as_bytes()[..],
                MUTATION_OPERATION_TOOL_UNCERTAINTY_ACKNOWLEDGEMENT,
                sequence_to_sql(fact_sequence)?,
                time_to_sql(now)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO tool_uncertainty_acknowledgements (
                request_id, operation_fingerprint, session_id, run_id,
                fact_sequence, accepted_at_milliseconds, delivery_event_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                &request_id.as_bytes()[..],
                &fingerprint[..],
                &session_id.as_bytes()[..],
                &run_id.as_bytes()[..],
                sequence_to_sql(fact_sequence)?,
                time_to_sql(now)?,
                &event_id[..],
            ],
        )?;
        transaction.execute(
            "INSERT INTO tool_audit_facts (
                audit_id, audit_sequence, call_id, request_id, session_id, run_id,
                operation_id, tool_kind, audit_kind, path_digest, created_at_milliseconds
             ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, NULL, NULL, 6, NULL, ?6)",
            params![
                &audit_id[..],
                sequence_to_sql(audit_sequence)?,
                &request_id.as_bytes()[..],
                &session_id.as_bytes()[..],
                &run_id.as_bytes()[..],
                time_to_sql(now)?,
            ],
        )?;
        insert_delivery_event(
            &transaction,
            &event_id,
            fact_sequence,
            session_id,
            EVENT_TOOL_UNCERTAINTY_CHANGED,
            now,
        )?;
        transaction.execute(
            "UPDATE sessions SET updated_sequence = ?1 WHERE session_id = ?2",
            params![sequence_to_sql(fact_sequence)?, &session_id.as_bytes()[..]],
        )?;
        transaction.execute(
            "UPDATE session_run_states SET updated_sequence = ?1 WHERE session_id = ?2",
            params![sequence_to_sql(fact_sequence)?, &session_id.as_bytes()[..]],
        )?;
        transaction.commit()?;
        Ok(ToolUncertaintyAcknowledgement {
            session_id,
            run_id,
            workspace: self.workspace_summary(session_id)?,
        })
    }
}

fn insert_assistant_commentary(
    transaction: &Transaction<'_>,
    run: &crate::persistence::Run,
    entry_sequence: u64,
    text: String,
    refusal: bool,
    now: u64,
) -> Result<u64, PersistenceError> {
    if text.is_empty() || text.len() > MAX_TRANSCRIPT_TEXT_BYTES {
        return Err(PersistenceError::InvalidInput {
            reason: "provider commentary is invalid",
        });
    }
    let fact_id = random_identifier()?;
    let message_id = MessageId::from_bytes(random_identifier()?);
    let event_id = random_identifier()?;
    let fact_sequence = next_sequence(transaction)?;
    transaction.execute(
        "INSERT INTO session_entries (
            fact_id, fact_sequence, session_id, entry_sequence, message_id, run_id,
            entry_kind, actor_kind, open_code_service, model_id, text, refusal,
            assistant_phase, tool_call_id, created_at_milliseconds, delivery_event_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 2, 2, ?7, ?8, ?9, ?10, 1, NULL, ?11, ?12)",
        params![
            &fact_id[..],
            sequence_to_sql(fact_sequence)?,
            &run.session_id.as_bytes()[..],
            sequence_to_sql(entry_sequence)?,
            &message_id.as_bytes()[..],
            &run.id.as_bytes()[..],
            run.service.to_record(),
            &run.model_id,
            &text,
            refusal,
            time_to_sql(now)?,
            &event_id[..],
        ],
    )?;
    insert_delivery_event(
        transaction,
        &event_id,
        fact_sequence,
        run.session_id,
        EVENT_ASSISTANT_MESSAGE,
        now,
    )?;
    Ok(fact_sequence)
}

fn validate_tool_turn(turn: &CompletedToolTurn) -> Result<(), PersistenceError> {
    if turn.provider_response_id.is_empty()
        || turn.provider_response_id.len() > 128
        || !turn
            .provider_response_id
            .bytes()
            .all(|byte| (0x21..=0x7e).contains(&byte))
        || turn.calls.is_empty()
        || turn.calls.len() > crate::tools::MAX_TOOL_CALLS_PER_TURN
        || turn
            .commentary
            .as_ref()
            .is_some_and(|(text, _)| text.is_empty() || text.len() > MAX_TRANSCRIPT_TEXT_BYTES)
    {
        return Err(PersistenceError::InvalidInput {
            reason: "a completed provider tool turn is invalid",
        });
    }
    Ok(())
}

fn provider_source_high_water(
    transaction: &Transaction<'_>,
    operation_id: ProviderOperationId,
) -> Result<u64, PersistenceError> {
    let value = transaction.query_row(
        "SELECT source_entry_high_water FROM provider_operation_facts
         WHERE operation_id = ?1 AND fact_kind = 1",
        [&operation_id.as_bytes()[..]],
        |row| row.get::<_, i64>(0),
    )?;
    u64::try_from(value).map_err(|_| PersistenceError::InvalidState {
        reason: "a provider operation source high water is invalid",
    })
}

struct ToolCallIdentity {
    call_id: ToolCallId,
    operation_id: ToolOperationId,
    session_id: crate::persistence::SessionId,
    run_id: RunId,
    kind: ToolKind,
    path_digest: [u8; 32],
}

fn load_tool_call_identity(
    transaction: &Transaction<'_>,
    call_id: ToolCallId,
) -> Result<ToolCallIdentity, PersistenceError> {
    transaction
        .query_row(
            "SELECT operation_id, session_id, run_id, tool_kind, path_digest
             FROM tool_calls WHERE call_id = ?1",
            [&call_id.as_bytes()[..]],
            |row| {
                let kind = ToolKind::from_record(row.get(3)?).ok_or_else(|| {
                    rusqlite::Error::InvalidColumnType(
                        3,
                        "tool_kind".to_owned(),
                        rusqlite::types::Type::Integer,
                    )
                })?;
                Ok(ToolCallIdentity {
                    call_id,
                    operation_id: ToolOperationId::from_bytes(row.get(0)?),
                    session_id: crate::persistence::SessionId::from_bytes(row.get(1)?),
                    run_id: RunId::from_bytes(row.get(2)?),
                    kind,
                    path_digest: row.get(4)?,
                })
            },
        )
        .optional()?
        .ok_or(PersistenceError::InvalidState {
            reason: "a tool operation is missing its committed call",
        })
}

fn require_active_tool_call(
    transaction: &Transaction<'_>,
    run: &crate::persistence::Run,
    call_id: ToolCallId,
    operation_id: ToolOperationId,
) -> Result<(), PersistenceError> {
    if run.state != RunState::Active {
        return Err(PersistenceError::InvalidState {
            reason: "a tool operation requires an active run",
        });
    }
    let call = load_tool_call_identity(transaction, call_id)?;
    if call.run_id != run.id
        || call.session_id != run.session_id
        || call.operation_id != operation_id
    {
        return Err(PersistenceError::InvalidState {
            reason: "a tool operation identity conflicts with its run",
        });
    }
    Ok(())
}

fn require_prior_calls_terminal(
    transaction: &Transaction<'_>,
    run_id: RunId,
    call_id: ToolCallId,
) -> Result<(), PersistenceError> {
    let incomplete: bool = transaction.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM tool_calls AS prior
            JOIN tool_calls AS current
              ON current.provider_operation_id = prior.provider_operation_id
            WHERE current.call_id = ?1 AND prior.run_id = ?2
              AND prior.call_index < current.call_index
              AND NOT EXISTS (
                  SELECT 1 FROM tool_operation_facts AS terminal
                  WHERE terminal.call_id = prior.call_id AND terminal.fact_kind BETWEEN 3 AND 6
              )
         )",
        params![&call_id.as_bytes()[..], &run_id.as_bytes()[..]],
        |row| row.get(0),
    )?;
    if incomplete {
        return Err(PersistenceError::InvalidState {
            reason: "tool calls must execute in committed provider order",
        });
    }
    Ok(())
}

fn require_tool_fact(
    transaction: &Transaction<'_>,
    call_id: ToolCallId,
    fact_kind: i64,
) -> Result<(), PersistenceError> {
    if !tool_has_fact(transaction, call_id, fact_kind)? {
        return Err(PersistenceError::InvalidState {
            reason: "a tool operation is missing a required fact",
        });
    }
    Ok(())
}

fn tool_has_fact(
    transaction: &Transaction<'_>,
    call_id: ToolCallId,
    fact_kind: i64,
) -> Result<bool, PersistenceError> {
    Ok(transaction.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM tool_operation_facts WHERE call_id = ?1 AND fact_kind = ?2
         )",
        params![&call_id.as_bytes()[..], fact_kind],
        |row| row.get(0),
    )?)
}

fn ensure_tool_not_terminal(
    transaction: &Transaction<'_>,
    call_id: ToolCallId,
) -> Result<(), PersistenceError> {
    let terminal: bool = transaction.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM tool_operation_facts
            WHERE call_id = ?1 AND fact_kind BETWEEN 3 AND 6
         )",
        [&call_id.as_bytes()[..]],
        |row| row.get(0),
    )?;
    if terminal {
        return Err(PersistenceError::InvalidState {
            reason: "a tool operation already has a terminal result",
        });
    }
    Ok(())
}

fn classify_result(result: &ToolResult, dispatched: bool) -> Result<(i64, i64), PersistenceError> {
    match result {
        ToolResult::Ok { .. } if dispatched => Ok((TOOL_FACT_COMPLETED, TOOL_RESULT_SUCCEEDED)),
        ToolResult::Error {
            error: ToolErrorKind::Uncertain,
        } if dispatched => Ok((TOOL_FACT_UNCERTAIN, TOOL_RESULT_UNCERTAIN)),
        ToolResult::Error {
            error: ToolErrorKind::Interrupted | ToolErrorKind::Cancelled,
        } if dispatched => Ok((TOOL_FACT_INTERRUPTED, TOOL_RESULT_INTERRUPTED)),
        ToolResult::Error {
            error:
                ToolErrorKind::NotDispatched | ToolErrorKind::Interrupted | ToolErrorKind::Cancelled,
        } if !dispatched => Ok((TOOL_FACT_NOT_DISPATCHED, TOOL_RESULT_INTERRUPTED)),
        ToolResult::Error { .. } => Ok((TOOL_FACT_COMPLETED, TOOL_RESULT_FAILED)),
        ToolResult::Ok { .. } => Err(PersistenceError::InvalidState {
            reason: "a successful tool result was not dispatched",
        }),
    }
}

fn insert_tool_audit(
    transaction: &Transaction<'_>,
    audit_id: &[u8; 16],
    audit_sequence: u64,
    call: &ToolCallIdentity,
    audit_kind: i64,
    now: u64,
) -> Result<(), PersistenceError> {
    transaction.execute(
        "INSERT INTO tool_audit_facts (
            audit_id, audit_sequence, call_id, request_id, session_id, run_id,
            operation_id, tool_kind, audit_kind, path_digest, created_at_milliseconds
         ) VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            &audit_id[..],
            sequence_to_sql(audit_sequence)?,
            &call.call_id.as_bytes()[..],
            &call.session_id.as_bytes()[..],
            &call.run_id.as_bytes()[..],
            &call.operation_id.as_bytes()[..],
            call.kind.to_record(),
            audit_kind,
            &call.path_digest[..],
            time_to_sql(now)?,
        ],
    )?;
    Ok(())
}

fn update_entry_high_water(
    transaction: &Transaction<'_>,
    run: &crate::persistence::Run,
    entry_high_water: u64,
    update_sequence: u64,
) -> Result<(), PersistenceError> {
    transaction.execute(
        "UPDATE session_run_states
         SET entry_high_water = ?1, updated_sequence = ?2
         WHERE session_id = ?3",
        params![
            sequence_to_sql(entry_high_water)?,
            sequence_to_sql(update_sequence)?,
            &run.session_id.as_bytes()[..],
        ],
    )?;
    transaction.execute(
        "UPDATE sessions SET updated_sequence = ?1 WHERE session_id = ?2",
        params![
            sequence_to_sql(update_sequence)?,
            &run.session_id.as_bytes()[..]
        ],
    )?;
    Ok(())
}

fn encode_payload<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, PersistenceError> {
    let payload = serde_json::to_vec(value).map_err(|_| PersistenceError::InvalidInput {
        reason: "a typed tool payload could not be encoded",
    })?;
    if payload.len() < 2 || payload.len() > MAX_TOOL_PAYLOAD_BYTES {
        return Err(limit());
    }
    Ok(payload)
}

const fn limit() -> PersistenceError {
    PersistenceError::ResourceLimit {
        resource: PersistenceResourceLimit::Context,
    }
}

const fn transcript_limit() -> PersistenceError {
    PersistenceError::ResourceLimit {
        resource: PersistenceResourceLimit::Transcript,
    }
}
