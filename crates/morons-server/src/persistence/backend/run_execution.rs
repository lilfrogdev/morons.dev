pub(super) mod facts;

use self::facts::*;
use rusqlite::{TransactionBehavior, params};

use super::{
    Backend,
    records::{
        current_time_milliseconds, next_sequence, random_identifier, sequence_to_sql, time_to_sql,
    },
    run_acceptance::insert_delivery_event,
    run_records::{
        AUDIT_PROVIDER_COMPLETED, AUDIT_PROVIDER_DISPATCHED, AUDIT_PROVIDER_FAILED,
        AUDIT_PROVIDER_PREPARED, AUDIT_RUN_ACTIVE, AUDIT_RUN_FAILED, AUDIT_RUN_STOPPED,
        AUDIT_RUN_SUCCEEDED, EVENT_ASSISTANT_MESSAGE, PROVIDER_FACT_ABANDONED,
        PROVIDER_FACT_DISPATCHED, PROVIDER_FACT_FAILED, PROVIDER_FACT_PREPARED,
        PROVIDER_FACT_UNCERTAIN, load_required_run,
    },
};
use crate::persistence::{
    MessageId, PersistenceError, PersistenceResourceLimit, Run, RunFailureKind, RunId, RunState,
    run_types::{
        ActivationOutcome, CompletedAssistant, DispatchOutcome, MAX_TRANSCRIPT_ENTRIES,
        PrepareOperationOutcome, ProviderOperationFailureState, ProviderOperationId,
    },
};

impl Backend {
    pub(crate) fn activate_run(
        &mut self,
        run_id: RunId,
    ) -> Result<ActivationOutcome, PersistenceError> {
        let active_transition = TransitionIdentifiers::generate()?;
        let terminal_transition = TransitionIdentifiers::generate()?;
        let now = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = load_required_run(&transaction, run_id)?;
        if run.state.is_terminal() {
            return Ok(ActivationOutcome::Terminal);
        }
        if run.cancellation_requested {
            if run.state == RunState::Accepted {
                append_run_transition(
                    &transaction,
                    &run,
                    RunState::Active,
                    None,
                    active_transition,
                    now,
                    AUDIT_RUN_ACTIVE,
                )?;
            }
            append_run_transition(
                &transaction,
                &run,
                RunState::Cancelled,
                None,
                terminal_transition,
                now,
                AUDIT_RUN_STOPPED,
            )?;
            transaction.commit()?;
            return Ok(ActivationOutcome::Terminal);
        }
        match run.state {
            RunState::Accepted => {
                append_run_transition(
                    &transaction,
                    &run,
                    RunState::Active,
                    None,
                    active_transition,
                    now,
                    AUDIT_RUN_ACTIVE,
                )?;
                transaction.commit()?;
                Ok(ActivationOutcome::Active)
            }
            RunState::Active => Ok(ActivationOutcome::Active),
            RunState::Succeeded
            | RunState::Failed
            | RunState::Cancelled
            | RunState::Interrupted
            | RunState::Uncertain => Ok(ActivationOutcome::Terminal),
        }
    }

    pub(crate) fn prepare_provider_operation(
        &mut self,
        run_id: RunId,
        source_entry_high_water: u64,
        estimated_input_tokens: u32,
    ) -> Result<PrepareOperationOutcome, PersistenceError> {
        let operation_id = ProviderOperationId::from_bytes(random_identifier()?);
        let operation_fact_id = random_identifier()?;
        let operation_audit_id = random_identifier()?;
        let transition = TransitionIdentifiers::generate()?;
        let now = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = load_required_run(&transaction, run_id)?;
        if run.state.is_terminal() {
            return Ok(PrepareOperationOutcome::Terminal);
        }
        if run.cancellation_requested {
            append_run_transition(
                &transaction,
                &run,
                RunState::Cancelled,
                None,
                transition,
                now,
                AUDIT_RUN_STOPPED,
            )?;
            transaction.commit()?;
            return Ok(PrepareOperationOutcome::Cancelled);
        }
        if run.state != RunState::Active {
            return Err(PersistenceError::InvalidState {
                reason: "a provider operation requires an active run",
            });
        }
        let current_entry_high_water = load_entry_high_water(&transaction, run.session_id)?;
        if current_entry_high_water != source_entry_high_water
            || estimated_input_tokens == 0
            || estimated_input_tokens > run.maximum_input_tokens
        {
            return Err(PersistenceError::InvalidState {
                reason: "a provider operation context changed before preparation",
            });
        }
        let operation_pending: bool = transaction.query_row(
            "SELECT EXISTS (
                SELECT 1 FROM provider_operation_facts AS prepared
                WHERE prepared.run_id = ?1 AND prepared.fact_kind = 1
                  AND NOT EXISTS (
                      SELECT 1 FROM provider_operation_facts AS terminal
                      WHERE terminal.operation_id = prepared.operation_id
                        AND terminal.fact_kind IN (3, 4, 5, 6)
                  )
             )",
            [&run_id.as_bytes()[..]],
            |row| row.get(0),
        )?;
        let turn_index =
            run.provider_turns
                .checked_add(1)
                .ok_or(PersistenceError::ResourceLimit {
                    resource: PersistenceResourceLimit::Context,
                })?;
        if operation_pending || turn_index > crate::tools::MAX_PROVIDER_TURNS_PER_RUN {
            return Err(PersistenceError::InvalidState {
                reason: "a run cannot prepare another provider operation",
            });
        }
        let fact_sequence = next_sequence(&transaction)?;
        let audit_sequence = next_sequence(&transaction)?;
        insert_provider_prepared(
            &transaction,
            ProviderPreparedFact {
                fact_id: &operation_fact_id,
                fact_sequence,
                operation_id,
                run: &run,
                turn_index,
                source_entry_high_water,
                estimated_input_tokens,
                created_at_milliseconds: now,
            },
        )?;
        insert_run_audit(
            &transaction,
            &operation_audit_id,
            audit_sequence,
            &run,
            Some(operation_id),
            AUDIT_PROVIDER_PREPARED,
            now,
        )?;
        transaction.commit()?;
        Ok(PrepareOperationOutcome::Prepared(operation_id))
    }

    pub(crate) fn mark_provider_dispatched(
        &mut self,
        run_id: RunId,
        operation_id: ProviderOperationId,
    ) -> Result<DispatchOutcome, PersistenceError> {
        let operation_fact_id = random_identifier()?;
        let operation_audit_id = random_identifier()?;
        let transition = TransitionIdentifiers::generate()?;
        let now = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = load_required_run(&transaction, run_id)?;
        if run.state.is_terminal() {
            return Ok(DispatchOutcome::Terminal);
        }
        require_provider_fact(&transaction, run_id, operation_id, PROVIDER_FACT_PREPARED)?;
        if run.cancellation_requested {
            let operation_sequence = next_sequence(&transaction)?;
            let operation_audit_sequence = next_sequence(&transaction)?;
            insert_provider_simple_fact(
                &transaction,
                ProviderSimpleFact {
                    fact_id: operation_fact_id,
                    fact_sequence: operation_sequence,
                    operation_id,
                    run_id,
                    fact_kind: PROVIDER_FACT_ABANDONED,
                    failure: None,
                    created_at_milliseconds: now,
                },
            )?;
            insert_run_audit(
                &transaction,
                &operation_audit_id,
                operation_audit_sequence,
                &run,
                Some(operation_id),
                AUDIT_PROVIDER_FAILED,
                now,
            )?;
            append_run_transition(
                &transaction,
                &run,
                RunState::Cancelled,
                None,
                transition,
                now,
                AUDIT_RUN_STOPPED,
            )?;
            transaction.commit()?;
            return Ok(DispatchOutcome::Cancelled);
        }
        if run.state != RunState::Active {
            return Err(PersistenceError::InvalidState {
                reason: "provider dispatch requires an active run",
            });
        }
        ensure_provider_not_terminal(&transaction, operation_id)?;
        let fact_sequence = next_sequence(&transaction)?;
        let audit_sequence = next_sequence(&transaction)?;
        insert_provider_simple_fact(
            &transaction,
            ProviderSimpleFact {
                fact_id: operation_fact_id,
                fact_sequence,
                operation_id,
                run_id,
                fact_kind: PROVIDER_FACT_DISPATCHED,
                failure: None,
                created_at_milliseconds: now,
            },
        )?;
        insert_run_audit(
            &transaction,
            &operation_audit_id,
            audit_sequence,
            &run,
            Some(operation_id),
            AUDIT_PROVIDER_DISPATCHED,
            now,
        )?;
        transaction.commit()?;
        Ok(DispatchOutcome::Dispatched)
    }

    pub(crate) fn complete_run_success(
        &mut self,
        run_id: RunId,
        operation_id: ProviderOperationId,
        assistant: CompletedAssistant,
    ) -> Result<Run, PersistenceError> {
        validate_completed_assistant(&assistant)?;
        let operation_fact_id = random_identifier()?;
        let operation_audit_id = random_identifier()?;
        let assistant_fact_id = random_identifier()?;
        let assistant_message_id = MessageId::from_bytes(random_identifier()?);
        let assistant_delivery_event_id = random_identifier()?;
        let transition = TransitionIdentifiers::generate()?;
        let now = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = load_required_run(&transaction, run_id)?;
        if run.state.is_terminal() {
            return Ok(run);
        }
        require_dispatched_active_operation(&transaction, &run, operation_id)?;
        let operation_sequence = next_sequence(&transaction)?;
        let operation_audit_sequence = next_sequence(&transaction)?;
        insert_provider_completed(
            &transaction,
            ProviderCompletedFact {
                fact_id: &operation_fact_id,
                fact_sequence: operation_sequence,
                operation_id,
                run_id,
                provider_response_id: &assistant.provider_response_id,
                usage: assistant.usage,
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

        if run.cancellation_requested {
            append_run_transition(
                &transaction,
                &run,
                RunState::Cancelled,
                None,
                transition,
                now,
                AUDIT_RUN_STOPPED,
            )?;
            transaction.commit()?;
            return load_required_run(&self.connection, run_id);
        }

        let expected_entry_high_water: i64 = transaction.query_row(
            "SELECT source_entry_high_water FROM provider_operation_facts
             WHERE operation_id = ?1 AND fact_kind = 1",
            [&operation_id.as_bytes()[..]],
            |row| row.get(0),
        )?;
        let expected_entry_high_water = u64::try_from(expected_entry_high_water).map_err(|_| {
            PersistenceError::InvalidState {
                reason: "a provider operation source high water is invalid",
            }
        })?;
        let entry_high_water = load_entry_high_water(&transaction, run.session_id)?;
        if entry_high_water != expected_entry_high_water {
            return Err(PersistenceError::InvalidState {
                reason: "a run transcript changed before its assistant outcome committed",
            });
        }
        let assistant_entry_sequence = entry_high_water
            .checked_add(1)
            .filter(|entry_sequence| *entry_sequence <= MAX_TRANSCRIPT_ENTRIES)
            .ok_or(PersistenceError::ResourceLimit {
                resource: PersistenceResourceLimit::Transcript,
            })?;
        let assistant_fact_sequence = next_sequence(&transaction)?;
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
                assistant_phase,
                created_at_milliseconds,
                delivery_event_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 2, 2, ?7, ?8, ?9, ?10, 2, ?11, ?12)",
            params![
                &assistant_fact_id[..],
                sequence_to_sql(assistant_fact_sequence)?,
                &run.session_id.as_bytes()[..],
                sequence_to_sql(assistant_entry_sequence)?,
                &assistant_message_id.as_bytes()[..],
                &run.id.as_bytes()[..],
                run.service.to_record(),
                &run.model_id,
                &assistant.text,
                assistant.refusal,
                time_to_sql(now)?,
                &assistant_delivery_event_id[..],
            ],
        )?;
        insert_delivery_event(
            &transaction,
            &assistant_delivery_event_id,
            assistant_fact_sequence,
            run.session_id,
            EVENT_ASSISTANT_MESSAGE,
            now,
        )?;
        transaction.execute(
            "UPDATE runs SET provider_turns = provider_turns + 1 WHERE run_id = ?1",
            [&run.id.as_bytes()[..]],
        )?;
        transaction.execute(
            "UPDATE session_run_states SET entry_high_water = ?1 WHERE session_id = ?2",
            params![
                sequence_to_sql(assistant_entry_sequence)?,
                &run.session_id.as_bytes()[..]
            ],
        )?;
        append_run_transition(
            &transaction,
            &run,
            RunState::Succeeded,
            None,
            transition,
            now,
            AUDIT_RUN_SUCCEEDED,
        )?;
        transaction.commit()?;
        load_required_run(&self.connection, run_id)
    }

    pub(crate) fn finish_run_failure(
        &mut self,
        run_id: RunId,
        operation_id: Option<ProviderOperationId>,
        failure: RunFailureKind,
        operation_state: ProviderOperationFailureState,
    ) -> Result<Run, PersistenceError> {
        self.finish_run(
            run_id,
            operation_id,
            Some(failure),
            Some(operation_state),
            FinishKind::Failed,
        )
    }

    pub(crate) fn finish_run_stopped(
        &mut self,
        run_id: RunId,
        operation_id: Option<ProviderOperationId>,
    ) -> Result<Run, PersistenceError> {
        self.finish_run(run_id, operation_id, None, None, FinishKind::Stopped)
    }

    fn finish_run(
        &mut self,
        run_id: RunId,
        operation_id: Option<ProviderOperationId>,
        failure: Option<RunFailureKind>,
        operation_state: Option<ProviderOperationFailureState>,
        finish_kind: FinishKind,
    ) -> Result<Run, PersistenceError> {
        let operation_fact_id = random_identifier()?;
        let operation_audit_id = random_identifier()?;
        let transition = TransitionIdentifiers::generate()?;
        let now = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = load_required_run(&transaction, run_id)?;
        if run.state.is_terminal() {
            return Ok(run);
        }
        if !matches!(run.state, RunState::Accepted | RunState::Active) {
            return Err(PersistenceError::InvalidState {
                reason: "a run cannot terminate from its current state",
            });
        }

        if let Some(operation_id) = operation_id {
            require_provider_fact(&transaction, run_id, operation_id, PROVIDER_FACT_PREPARED)?;
            ensure_provider_not_terminal(&transaction, operation_id)?;
            let dispatched =
                provider_has_fact(&transaction, operation_id, PROVIDER_FACT_DISPATCHED)?;
            let (fact_kind, operation_failure) = match finish_kind {
                FinishKind::Stopped if dispatched => (PROVIDER_FACT_UNCERTAIN, None),
                FinishKind::Stopped => (PROVIDER_FACT_ABANDONED, None),
                FinishKind::Failed => {
                    match operation_state.ok_or(PersistenceError::InvalidState {
                        reason: "a failed provider operation is missing its outcome state",
                    })? {
                        ProviderOperationFailureState::Failed => (PROVIDER_FACT_FAILED, failure),
                        ProviderOperationFailureState::Uncertain => {
                            (PROVIDER_FACT_UNCERTAIN, failure)
                        }
                    }
                }
            };
            let operation_sequence = next_sequence(&transaction)?;
            let operation_audit_sequence = next_sequence(&transaction)?;
            insert_provider_simple_fact(
                &transaction,
                ProviderSimpleFact {
                    fact_id: operation_fact_id,
                    fact_sequence: operation_sequence,
                    operation_id,
                    run_id,
                    fact_kind,
                    failure: operation_failure,
                    created_at_milliseconds: now,
                },
            )?;
            insert_run_audit(
                &transaction,
                &operation_audit_id,
                operation_audit_sequence,
                &run,
                Some(operation_id),
                AUDIT_PROVIDER_FAILED,
                now,
            )?;
        }

        let (terminal_state, terminal_failure, audit_kind) = match finish_kind {
            FinishKind::Failed => (
                RunState::Failed,
                Some(failure.ok_or(PersistenceError::InvalidState {
                    reason: "a failed run is missing its failure classification",
                })?),
                AUDIT_RUN_FAILED,
            ),
            FinishKind::Stopped if run.cancellation_requested => {
                (RunState::Cancelled, None, AUDIT_RUN_STOPPED)
            }
            FinishKind::Stopped => (RunState::Interrupted, None, AUDIT_RUN_STOPPED),
        };
        append_run_transition(
            &transaction,
            &run,
            terminal_state,
            terminal_failure,
            transition,
            now,
            audit_kind,
        )?;
        transaction.commit()?;
        load_required_run(&self.connection, run_id)
    }
}
