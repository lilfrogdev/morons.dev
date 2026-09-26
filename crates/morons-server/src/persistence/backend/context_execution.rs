//! ADR0052 execution policy; independent of canonical source digest version4.
#[cfg(test)]
mod tests;
use super::context_budget::{CONTEXT_ITEM_RESERVE, ContextBudget, MAX_ACTIVE_CONTEXT_ENTRIES};
use crate::persistence::{PersistenceError, RunId};
use rusqlite::{Connection, params};

pub(super) const MAX_SOURCE_BYTES: u64 = 1024 * 1024;
pub(super) const MAX_COMPACTIONS: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::persistence) enum ExecutionPolicy {
    Legacy,
    ConservativeRepeated,
    NativeUsage,
}

pub(in crate::persistence) fn policy(
    connection: &Connection,
    run: RunId,
) -> Result<ExecutionPolicy, PersistenceError> {
    let enabled: bool = connection.query_row(
        "SELECT accepted.fact_sequence >= epoch.first_sequence
          AND accepted.open_code_service = 3 AND accepted.protocol_revision = 5
          AND accepted.context_policy_version = 4
          AND accepted.tool_catalog_version IN (13, 14, 15, 16)
          AND ((accepted.tool_catalog_version IN (13, 14) AND accepted.tool_limits_version = accepted.tool_catalog_version)
               OR (accepted.tool_catalog_version IN (13, 15, 16) AND accepted.tool_limits_version = 14))
          AND (accepted.maximum_input_tokens = 96000
               OR (accepted.maximum_input_tokens = 258400 AND accepted.tool_catalog_version = 16 AND accepted.tool_limits_version = 14))
          AND accepted.maximum_output_tokens = 32000
          AND accepted.model_id IN ('gpt-5.5','gpt-6-astra','gpt-5.6-sol','gpt-5.6-luna','gpt-5.6-terra','gpt-daybreak-blue-latest')
         FROM run_accepted_facts AS accepted CROSS JOIN context_accounting_epoch AS epoch
         WHERE accepted.run_id = ?1 AND epoch.singleton = 1",
        [&run.as_bytes()[..]], |r| r.get(0))?;
    let repeated: bool = connection.query_row(
        "SELECT accepted.fact_sequence >= epoch.repeated_first_sequence
          AND accepted.open_code_service IN (1, 2) AND accepted.protocol_revision BETWEEN 1 AND 4
          AND accepted.context_policy_version = 4
          AND accepted.tool_catalog_version IN (14, 15, 16) AND accepted.tool_limits_version = 14
          AND accepted.maximum_input_tokens = 96000 AND accepted.maximum_output_tokens = 32000
         FROM run_accepted_facts AS accepted CROSS JOIN context_accounting_epoch AS epoch
         WHERE accepted.run_id = ?1 AND epoch.singleton = 1",
        [&run.as_bytes()[..]],
        |r| r.get(0),
    )?;
    Ok(if enabled {
        ExecutionPolicy::NativeUsage
    } else if repeated {
        ExecutionPolicy::ConservativeRepeated
    } else {
        ExecutionPolicy::Legacy
    })
}

impl ExecutionPolicy {
    pub(super) fn allows_within_run_compaction(self) -> bool {
        self != Self::Legacy
    }

    pub(super) fn estimate(self, budget: &ContextBudget, extra: usize) -> u64 {
        if self == Self::NativeUsage && budget.images == 0 {
            budget.estimated_tokens(extra)
        } else {
            budget.tokens(extra)
        }
    }

    pub(super) fn fits(self, budget: &ContextBudget, maximum: u32, extra: usize) -> bool {
        if self != Self::NativeUsage {
            return budget.fits(maximum, extra);
        }
        self.estimate(budget, extra) <= u64::from(maximum)
            && budget.bytes.saturating_add(extra as u64) <= MAX_SOURCE_BYTES
            && budget
                .entries
                .saturating_add(budget.images)
                .saturating_add(CONTEXT_ITEM_RESERVE)
                <= MAX_ACTIVE_CONTEXT_ENTRIES as u64
            && budget.images <= crate::persistence::images::MAX_CONTEXT_IMAGES as u64
            && budget.image_bytes <= crate::persistence::images::MAX_CONTEXT_IMAGE_BYTES
    }

    pub(super) fn log_rejected_budget(
        self,
        run: RunId,
        budget: &ContextBudget,
        maximum: u32,
        extra: usize,
    ) {
        for event in self.rejected_budget_events(run, budget, maximum, extra) {
            crate::debug_log::emit(event);
        }
    }

    fn rejected_budget_events(
        self,
        run: RunId,
        budget: &ContextBudget,
        maximum: u32,
        extra: usize,
    ) -> impl Iterator<Item = crate::debug_log::DebugEvent> {
        use crate::debug_log::{DebugContextCheck as Check, DebugEvent};
        let tokens = if self == Self::NativeUsage {
            self.estimate(budget, extra)
        } else {
            budget.tokens(extra)
        };
        let checks = [
            (Check::InputTokens, tokens, u64::from(maximum)),
            (
                Check::SourceBytes,
                budget.bytes.saturating_add(extra as u64),
                if self == Self::NativeUsage {
                    MAX_SOURCE_BYTES
                } else {
                    u64::MAX
                },
            ),
            (
                Check::ReservedEntries,
                budget
                    .entries
                    .saturating_add(budget.images)
                    .saturating_add(CONTEXT_ITEM_RESERVE),
                MAX_ACTIVE_CONTEXT_ENTRIES as u64,
            ),
            (
                Check::ImageCount,
                budget.images,
                crate::persistence::images::MAX_CONTEXT_IMAGES as u64,
            ),
            (
                Check::ImageBytes,
                budget.image_bytes,
                crate::persistence::images::MAX_CONTEXT_IMAGE_BYTES,
            ),
        ];
        checks
            .into_iter()
            .filter_map(move |(check, measured, limit)| {
                (measured > limit).then_some(DebugEvent::ContextLimit {
                    run_id: *run.as_bytes(),
                    call_id: None,
                    check,
                    measured,
                    limit,
                })
            })
    }

    pub(super) fn token_pressure_threshold(self, maximum: u32) -> u32 {
        if self == Self::NativeUsage
            && maximum == crate::provider::openai_codex::USABLE_INPUT_TOKENS
        {
            crate::provider::openai_codex::AUTO_COMPACT_TOKENS
        } else {
            maximum.saturating_mul(7) / 10
        }
    }

    pub(super) fn pressure(self, budget: &ContextBudget, maximum: u32, extra: usize) -> bool {
        if self != Self::NativeUsage {
            return budget.pressure(maximum, extra);
        }
        let token_pressure = u64::from(self.token_pressure_threshold(maximum));
        !self.fits(budget, maximum, extra)
            || self.estimate(budget, extra) >= token_pressure
            || budget.bytes.saturating_add(extra as u64) >= MAX_SOURCE_BYTES * 3 / 4
            || budget
                .entries
                .saturating_add(budget.images)
                .saturating_add(CONTEXT_ITEM_RESERVE)
                >= 192
            || budget.images >= crate::persistence::images::MAX_CONTEXT_IMAGES as u64 * 3 / 4
            || budget.image_bytes >= crate::persistence::images::MAX_CONTEXT_IMAGE_BYTES * 3 / 4
    }
}

/// No completed batch cut may leave a call without its result in the same prefix.
pub(in crate::persistence) fn complete_cut(
    connection: &Connection,
    run: RunId,
    cut: u64,
) -> Result<bool, PersistenceError> {
    Ok(connection.query_row(
        "SELECT NOT EXISTS (
          SELECT 1 FROM tool_calls AS call JOIN session_entries AS start ON start.tool_call_id = call.call_id AND start.entry_kind = 3
          LEFT JOIN session_entries AS finish ON finish.tool_call_id = call.call_id AND finish.entry_kind = 4
          WHERE call.run_id = ?1 AND start.entry_sequence <= ?2 AND (finish.entry_sequence IS NULL OR finish.entry_sequence > ?2)
        ) AND NOT EXISTS (
          SELECT 1 FROM tool_calls AS a JOIN session_entries AS end_a ON end_a.tool_call_id = a.call_id AND end_a.entry_kind = 4
          JOIN tool_calls AS b ON b.provider_operation_id = a.provider_operation_id
          LEFT JOIN session_entries AS end_b ON end_b.tool_call_id = b.call_id AND end_b.entry_kind = 4
          WHERE a.run_id = ?1 AND end_a.entry_sequence <= ?2 AND (end_b.entry_sequence IS NULL OR end_b.entry_sequence > ?2)
        )", params![&run.as_bytes()[..], super::records::sequence_to_sql(cut)?], |r| r.get(0))?)
}

pub(super) fn can_compact(connection: &Connection, run: RunId) -> Result<bool, PersistenceError> {
    let (count, unfinished, last): (u32, bool, i64) = connection.query_row(
        "SELECT COUNT(*), COALESCE(MAX(state != 3),0), COALESCE(MAX(updated_sequence),0) FROM compaction_operations WHERE run_id = ?1",
        [&run.as_bytes()[..]], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let execution = policy(connection, run)?;
    if execution.allows_within_run_compaction() {
        let pending: bool = connection.query_row("SELECT EXISTS (SELECT 1 FROM provider_operation_facts AS prepared WHERE prepared.run_id = ?1 AND prepared.fact_kind = 1 AND NOT EXISTS (SELECT 1 FROM provider_operation_facts AS terminal WHERE terminal.operation_id = prepared.operation_id AND terminal.fact_kind BETWEEN 3 AND 6)) OR EXISTS (SELECT 1 FROM tool_calls AS call WHERE call.run_id = ?1 AND NOT EXISTS (SELECT 1 FROM session_entries AS result WHERE result.tool_call_id = call.call_id AND result.entry_kind = 4))", [&run.as_bytes()[..]], |r| r.get(0))?;
        if pending {
            return Ok(false);
        }
    }
    if count == 0 {
        return Ok(true);
    }
    if execution == ExecutionPolicy::Legacy || count >= MAX_COMPACTIONS || unfinished {
        return Ok(false);
    }
    Ok(connection.query_row("SELECT EXISTS (SELECT 1 FROM provider_operation_facts WHERE run_id = ?1 AND fact_kind = 3 AND fact_sequence > ?2)", params![&run.as_bytes()[..], last], |r| r.get(0))?)
}

pub(in crate::persistence) fn validate_operations(
    connection: &Connection,
) -> Result<(), PersistenceError> {
    validate_epoch(connection)?;
    let mut statement = connection.prepare("SELECT run_id, source_entry_high_water, prepared_sequence, updated_sequence, state FROM compaction_operations ORDER BY run_id, prepared_sequence")?;
    let rows = statement.query_map([], |r| {
        Ok((
            r.get::<_, [u8; 16]>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, u32>(4)?,
        ))
    })?;
    let mut previous = None;
    let mut count = 0;
    for row in rows {
        let (id, cut, prepared, updated, state) = row?;
        let run = RunId::from_bytes(id);
        let execution = policy(connection, run)?;
        let cut = u64::try_from(cut).map_err(|_| invalid())?;
        if !super::context_compaction::within_run::source_allowed(
            connection,
            run,
            cut,
            Some(prepared),
        )? {
            return Err(invalid());
        }
        if let Some((prior_id, prior_cut, prior_updated, prior_state)) = previous
            && prior_id == id
        {
            count += 1;
            if execution == ExecutionPolicy::Legacy
                || count > MAX_COMPACTIONS
                || prior_cut >= cut
                || prior_updated >= prepared
                || prior_state != 3
            {
                return Err(invalid());
            }
            let advanced: bool = connection.query_row("SELECT EXISTS (SELECT 1 FROM provider_operation_facts WHERE run_id = ?1 AND fact_kind = 3 AND fact_sequence > ?2 AND fact_sequence < ?3)", params![&id[..], prior_updated, prepared], |r| r.get(0))?;
            if !advanced {
                return Err(invalid());
            }
        } else {
            count = 1;
        }
        previous = Some((id, cut, updated, state));
    }
    Ok(())
}

fn invalid() -> PersistenceError {
    PersistenceError::InvalidState {
        reason: "context execution lineage is invalid",
    }
}

pub(in crate::persistence) fn validate_epoch(
    connection: &Connection,
) -> Result<(), PersistenceError> {
    let valid: bool = connection.query_row("SELECT COUNT(*) = 1 AND COALESCE(MAX(first_sequence),0) BETWEEN 1 AND (SELECT next_value FROM logical_sequences WHERE singleton = 1) AND COALESCE(MAX(repeated_first_sequence),0) BETWEEN MAX(first_sequence) AND (SELECT next_value FROM logical_sequences WHERE singleton = 1) FROM context_accounting_epoch", [], |r| r.get(0))?;
    if !valid {
        return Err(PersistenceError::InvalidState {
            reason: "context accounting epoch is invalid",
        });
    }
    Ok(())
}
