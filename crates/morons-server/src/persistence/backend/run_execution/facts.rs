use rusqlite::{OptionalExtension, Transaction, params};

use super::super::{
    records::{next_sequence, random_identifier, sequence_to_sql, time_to_sql},
    run_acceptance::insert_delivery_event,
    run_records::{
        EVENT_RUN_ACTIVE, EVENT_RUN_CANCELLED, EVENT_RUN_FAILED, EVENT_RUN_INTERRUPTED,
        EVENT_RUN_SUCCEEDED, PROVIDER_FACT_DISPATCHED, PROVIDER_FACT_PREPARED, RUN_STATE_ACTIVE,
        RUN_STATE_CANCELLED, RUN_STATE_FAILED, RUN_STATE_INTERRUPTED, RUN_STATE_SUCCEEDED,
    },
};
use crate::persistence::{
    CompletedAssistant, PersistenceError, ProviderUsage, Run, RunFailureKind, RunId, RunState,
    run_types::{MAX_TRANSCRIPT_TEXT_BYTES, ProviderOperationId},
};

#[derive(Clone, Copy)]
pub(super) enum FinishKind {
    Failed,
    Stopped,
}

#[derive(Clone, Copy)]
pub(crate) struct TransitionIdentifiers {
    fact_id: [u8; 16],
    delivery_event_id: [u8; 16],
    audit_id: [u8; 16],
}

impl TransitionIdentifiers {
    pub(crate) fn generate() -> Result<Self, PersistenceError> {
        Ok(Self {
            fact_id: random_identifier()?,
            delivery_event_id: random_identifier()?,
            audit_id: random_identifier()?,
        })
    }
}

pub(crate) fn append_run_transition(
    transaction: &Transaction<'_>,
    run: &Run,
    state: RunState,
    failure: Option<RunFailureKind>,
    identifiers: TransitionIdentifiers,
    now: u64,
    audit_kind: i64,
) -> Result<(), PersistenceError> {
    if !matches!(
        state,
        RunState::Active
            | RunState::Succeeded
            | RunState::Failed
            | RunState::Cancelled
            | RunState::Interrupted
            | RunState::Uncertain
    ) || (state == RunState::Failed) != failure.is_some()
    {
        return Err(PersistenceError::InvalidState {
            reason: "a run transition has an invalid terminal classification",
        });
    }
    let fact_sequence = next_sequence(transaction)?;
    let audit_sequence = next_sequence(transaction)?;
    transaction.execute(
        "INSERT INTO run_state_facts (
            fact_id,
            fact_sequence,
            session_id,
            run_id,
            state,
            failure_kind,
            created_at_milliseconds,
            delivery_event_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            &identifiers.fact_id[..],
            sequence_to_sql(fact_sequence)?,
            &run.session_id.as_bytes()[..],
            &run.id.as_bytes()[..],
            state.to_record(),
            failure.map(RunFailureKind::to_record),
            time_to_sql(now)?,
            &identifiers.delivery_event_id[..],
        ],
    )?;
    insert_delivery_event(
        transaction,
        &identifiers.delivery_event_id,
        fact_sequence,
        run.session_id,
        event_kind_for_state(state)?,
        now,
    )?;
    insert_run_audit(
        transaction,
        &identifiers.audit_id,
        audit_sequence,
        run,
        None,
        audit_kind,
        now,
    )?;
    transaction.execute(
        "UPDATE runs
         SET state = ?1,
             failure_kind = ?2,
             updated_sequence = ?3,
             updated_at_milliseconds = ?4
         WHERE run_id = ?5",
        params![
            state.to_record(),
            failure.map(RunFailureKind::to_record),
            sequence_to_sql(fact_sequence)?,
            time_to_sql(now)?,
            &run.id.as_bytes()[..],
        ],
    )?;
    transaction.execute(
        "UPDATE session_run_states
         SET active_run_id = ?1,
             updated_sequence = ?2
         WHERE session_id = ?3",
        params![
            (!state.is_terminal()).then_some(&run.id.as_bytes()[..]),
            sequence_to_sql(fact_sequence)?,
            &run.session_id.as_bytes()[..],
        ],
    )?;
    transaction.execute(
        "UPDATE sessions SET updated_sequence = ?1 WHERE session_id = ?2",
        params![
            sequence_to_sql(fact_sequence)?,
            &run.session_id.as_bytes()[..]
        ],
    )?;
    Ok(())
}

pub(super) struct ProviderPreparedFact<'a> {
    pub fact_id: &'a [u8; 16],
    pub fact_sequence: u64,
    pub operation_id: ProviderOperationId,
    pub run: &'a Run,
    pub turn_index: u16,
    pub source_entry_high_water: u64,
    pub estimated_input_tokens: u32,
    pub created_at_milliseconds: u64,
}

pub(super) fn insert_provider_prepared(
    transaction: &Transaction<'_>,
    fact: ProviderPreparedFact<'_>,
) -> Result<(), PersistenceError> {
    transaction.execute(
        "INSERT INTO provider_operation_facts (
            fact_id, fact_sequence, operation_id, run_id, fact_kind,
            open_code_service, model_id, protocol_revision,
            credential_generation, context_policy_version, source_entry_high_water,
            provider_response_id, failure_kind, input_tokens, cached_input_tokens,
            cache_write_input_tokens, output_tokens, reasoning_output_tokens, total_tokens,
            created_at_milliseconds, turn_index, tool_catalog_version,
            tool_limits_version, estimated_input_tokens
         ) VALUES (
            ?1, ?2, ?3, ?4, 1, ?5, ?6, ?7, ?8, ?9, ?10,
            NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, ?11, ?12, ?13, ?14, ?15
         )",
        params![
            &fact.fact_id[..],
            sequence_to_sql(fact.fact_sequence)?,
            &fact.operation_id.as_bytes()[..],
            &fact.run.id.as_bytes()[..],
            fact.run.service.to_record(),
            &fact.run.model_id,
            i64::from(fact.run.protocol_revision),
            sequence_to_sql(fact.run.credential_generation)?,
            i64::from(fact.run.context_policy_version),
            sequence_to_sql(fact.source_entry_high_water)?,
            time_to_sql(fact.created_at_milliseconds)?,
            i64::from(fact.turn_index),
            i64::from(fact.run.tool_catalog_version),
            i64::from(fact.run.tool_limits_version),
            i64::from(fact.estimated_input_tokens),
        ],
    )?;
    Ok(())
}

pub(crate) struct ProviderCompletedFact<'a> {
    pub fact_id: &'a [u8; 16],
    pub fact_sequence: u64,
    pub operation_id: ProviderOperationId,
    pub run_id: RunId,
    pub provider_response_id: &'a str,
    pub usage: ProviderUsage,
    pub created_at_milliseconds: u64,
}

pub(crate) fn insert_provider_completed(
    transaction: &Transaction<'_>,
    fact: ProviderCompletedFact<'_>,
) -> Result<(), PersistenceError> {
    transaction.execute(
        "INSERT INTO provider_operation_facts (
            fact_id, fact_sequence, operation_id, run_id, fact_kind,
            open_code_service, model_id, protocol_revision,
            credential_generation, context_policy_version, source_entry_high_water,
            provider_response_id, failure_kind, input_tokens, cached_input_tokens,
            cache_write_input_tokens, output_tokens, reasoning_output_tokens, total_tokens,
            created_at_milliseconds, turn_index, tool_catalog_version,
            tool_limits_version, estimated_input_tokens
         ) VALUES (
            ?1, ?2, ?3, ?4, 3,
            NULL, NULL, NULL, NULL, NULL, NULL,
            ?5, NULL, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL, NULL, NULL, NULL
         )",
        params![
            &fact.fact_id[..],
            sequence_to_sql(fact.fact_sequence)?,
            &fact.operation_id.as_bytes()[..],
            &fact.run_id.as_bytes()[..],
            fact.provider_response_id,
            usage_to_sql(fact.usage.input_tokens)?,
            usage_to_sql(fact.usage.cached_input_tokens)?,
            usage_to_sql(fact.usage.cache_write_input_tokens)?,
            usage_to_sql(fact.usage.output_tokens)?,
            usage_to_sql(fact.usage.reasoning_output_tokens)?,
            usage_to_sql(fact.usage.total_tokens)?,
            time_to_sql(fact.created_at_milliseconds)?,
        ],
    )?;
    Ok(())
}

pub(super) struct ProviderSimpleFact {
    pub(super) fact_id: [u8; 16],
    pub(super) fact_sequence: u64,
    pub(super) operation_id: ProviderOperationId,
    pub(super) run_id: RunId,
    pub(super) fact_kind: i64,
    pub(super) failure: Option<RunFailureKind>,
    pub(super) created_at_milliseconds: u64,
}

pub(super) fn insert_provider_simple_fact(
    transaction: &Transaction<'_>,
    fact: ProviderSimpleFact,
) -> Result<(), PersistenceError> {
    transaction.execute(
        "INSERT INTO provider_operation_facts (
            fact_id, fact_sequence, operation_id, run_id, fact_kind,
            open_code_service, model_id, protocol_revision,
            credential_generation, context_policy_version, source_entry_high_water,
            provider_response_id, failure_kind, input_tokens, cached_input_tokens,
            cache_write_input_tokens, output_tokens, reasoning_output_tokens, total_tokens,
            created_at_milliseconds
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5,
            NULL, NULL, NULL, NULL, NULL, NULL,
            NULL, ?6, NULL, NULL, NULL, NULL, NULL, NULL, ?7
         )",
        params![
            &fact.fact_id[..],
            sequence_to_sql(fact.fact_sequence)?,
            &fact.operation_id.as_bytes()[..],
            &fact.run_id.as_bytes()[..],
            fact.fact_kind,
            fact.failure.map(RunFailureKind::to_record),
            time_to_sql(fact.created_at_milliseconds)?,
        ],
    )?;
    Ok(())
}

pub(crate) fn insert_run_audit(
    transaction: &Transaction<'_>,
    audit_id: &[u8; 16],
    audit_sequence: u64,
    run: &Run,
    operation_id: Option<ProviderOperationId>,
    audit_kind: i64,
    now: u64,
) -> Result<(), PersistenceError> {
    transaction.execute(
        "INSERT INTO run_audit_facts (
            audit_id, audit_sequence, request_id, session_id, run_id,
            operation_id, actor_kind, audit_kind, created_at_milliseconds
         ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, 2, ?6, ?7)",
        params![
            &audit_id[..],
            sequence_to_sql(audit_sequence)?,
            &run.session_id.as_bytes()[..],
            &run.id.as_bytes()[..],
            operation_id.map(|id| *id.as_bytes()),
            audit_kind,
            time_to_sql(now)?,
        ],
    )?;
    Ok(())
}

pub(crate) fn require_dispatched_active_operation(
    transaction: &Transaction<'_>,
    run: &Run,
    operation_id: ProviderOperationId,
) -> Result<(), PersistenceError> {
    if run.state != RunState::Active {
        return Err(PersistenceError::InvalidState {
            reason: "a provider outcome requires an active run",
        });
    }
    require_provider_fact(transaction, run.id, operation_id, PROVIDER_FACT_PREPARED)?;
    require_provider_fact(transaction, run.id, operation_id, PROVIDER_FACT_DISPATCHED)?;
    ensure_provider_not_terminal(transaction, operation_id)
}

pub(super) fn require_provider_fact(
    transaction: &Transaction<'_>,
    run_id: RunId,
    operation_id: ProviderOperationId,
    fact_kind: i64,
) -> Result<(), PersistenceError> {
    let exists: bool = transaction.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM provider_operation_facts
            WHERE operation_id = ?1 AND run_id = ?2 AND fact_kind = ?3
         )",
        params![
            &operation_id.as_bytes()[..],
            &run_id.as_bytes()[..],
            fact_kind
        ],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(PersistenceError::InvalidState {
            reason: "a provider operation is missing a required durable fact",
        });
    }
    Ok(())
}

pub(super) fn ensure_provider_not_terminal(
    transaction: &Transaction<'_>,
    operation_id: ProviderOperationId,
) -> Result<(), PersistenceError> {
    let terminal: bool = transaction.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM provider_operation_facts
            WHERE operation_id = ?1 AND fact_kind IN (3, 4, 5, 6)
         )",
        [&operation_id.as_bytes()[..]],
        |row| row.get(0),
    )?;
    if terminal {
        return Err(PersistenceError::InvalidState {
            reason: "a provider operation already has a terminal fact",
        });
    }
    Ok(())
}

pub(super) fn provider_has_fact(
    transaction: &Transaction<'_>,
    operation_id: ProviderOperationId,
    fact_kind: i64,
) -> Result<bool, PersistenceError> {
    Ok(transaction.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM provider_operation_facts
            WHERE operation_id = ?1 AND fact_kind = ?2
         )",
        params![&operation_id.as_bytes()[..], fact_kind],
        |row| row.get(0),
    )?)
}

pub(crate) fn load_entry_high_water(
    transaction: &Transaction<'_>,
    session_id: crate::persistence::SessionId,
) -> Result<u64, PersistenceError> {
    let value = transaction
        .query_row(
            "SELECT entry_high_water FROM session_run_states WHERE session_id = ?1",
            [&session_id.as_bytes()[..]],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or(PersistenceError::InvalidState {
            reason: "a run session is missing its run-state projection",
        })?;
    u64::try_from(value).map_err(|_| PersistenceError::InvalidState {
        reason: "a session entry high water is invalid",
    })
}

pub(super) fn validate_completed_assistant(
    assistant: &CompletedAssistant,
) -> Result<(), PersistenceError> {
    if assistant.text.is_empty()
        || assistant.text.len() > MAX_TRANSCRIPT_TEXT_BYTES
        || assistant.provider_response_id.is_empty()
        || assistant.provider_response_id.len() > 128
        || !assistant
            .provider_response_id
            .bytes()
            .all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(PersistenceError::InvalidInput {
            reason: "a completed provider assistant outcome is invalid",
        });
    }
    Ok(())
}

fn event_kind_for_state(state: RunState) -> Result<i64, PersistenceError> {
    match state.to_record() {
        RUN_STATE_ACTIVE => Ok(EVENT_RUN_ACTIVE),
        RUN_STATE_SUCCEEDED => Ok(EVENT_RUN_SUCCEEDED),
        RUN_STATE_FAILED => Ok(EVENT_RUN_FAILED),
        RUN_STATE_CANCELLED => Ok(EVENT_RUN_CANCELLED),
        RUN_STATE_INTERRUPTED => Ok(EVENT_RUN_INTERRUPTED),
        super::super::run_records::RUN_STATE_UNCERTAIN => {
            Ok(super::super::run_records::EVENT_RUN_UNCERTAIN)
        }
        _ => Err(PersistenceError::InvalidState {
            reason: "the run state has no supported delivery event",
        }),
    }
}

fn usage_to_sql(value: u64) -> Result<i64, PersistenceError> {
    i64::try_from(value).map_err(|_| PersistenceError::InvalidInput {
        reason: "provider usage exceeds the supported durable range",
    })
}
