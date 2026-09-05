use rusqlite::{OptionalExtension as _, params};

use super::{Backend, records::sequence_to_sql, run_queries::load_run_skills};
use crate::{
    persistence::{
        ContextCheckpoint, PersistenceError, RunId, RunOpenCodeService, SessionId,
        run_types::RecentProviderUsage,
    },
    skills::RunSkillContext,
};

pub(super) struct ContextModel<'a> {
    pub service: RunOpenCodeService,
    pub model_id: &'a str,
    pub protocol_revision: u16,
}

pub(super) struct ContextObservation {
    pub estimated_tokens: u64,
    pub usage: RecentProviderUsage,
}

impl Backend {
    /// Reuse only a committed request for the same immutable prompt prefix.
    /// Unlike a tokenizer, this is advisory and never changes dispatch limits.
    pub(super) fn observe_context_usage(
        &self,
        session_id: SessionId,
        selection: ContextModel<'_>,
        checkpoint: Option<&ContextCheckpoint>,
        through: u64,
        skills: &RunSkillContext,
        project: Option<&crate::project_context::RunProjectContext>,
    ) -> Result<Option<ContextObservation>, PersistenceError> {
        // Bound lookup independently of retained history. CROSS JOIN keeps the
        // tiny run window first; both fact lookups use the existing run index.
        // Missing/older observations fall back, rather than scan the whole DB.
        let sample = self.connection.query_row(
            "WITH recent_runs AS (
                SELECT run_id FROM run_accepted_facts WHERE session_id = ?1 ORDER BY fact_sequence DESC LIMIT 8
             )
             SELECT prepared.run_id, prepared.source_entry_high_water,
                    completed.input_tokens, completed.cached_input_tokens,
                    completed.cache_write_input_tokens, completed.output_tokens,
                    completed.total_tokens, prepared.created_at_milliseconds,
                    completed.created_at_milliseconds
             FROM recent_runs AS accepted
             CROSS JOIN provider_operation_facts AS prepared ON prepared.run_id = accepted.run_id AND prepared.fact_kind = 1
             CROSS JOIN provider_operation_facts AS completed ON completed.run_id = prepared.run_id
                 AND completed.operation_id = prepared.operation_id AND completed.fact_kind = 3
             WHERE prepared.open_code_service = ?2 AND prepared.model_id = ?3
                 AND prepared.protocol_revision = ?4 AND prepared.context_policy_version = ?5
                 AND prepared.tool_catalog_version = ?6 AND prepared.tool_limits_version = ?7
                 AND prepared.source_entry_high_water <= ?8
                 AND prepared.fact_sequence > COALESCE((SELECT fact_sequence FROM context_checkpoints WHERE checkpoint_id = ?9), 0)
             ORDER BY completed.fact_sequence DESC LIMIT 1",
            params![&session_id.as_bytes()[..], selection.service.to_record(), selection.model_id,
                selection.protocol_revision, crate::persistence::CONTEXT_POLICY_VERSION,
                crate::tools::TOOL_CATALOG_VERSION, crate::tools::TOOL_LIMITS_VERSION,
                sequence_to_sql(through)?, checkpoint.map(|checkpoint| &checkpoint.id.as_bytes()[..])],
            |row| Ok((RunId::from_bytes(row.get(0)?), row.get::<_, i64>(1)?, row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?, row.get::<_, i64>(4)?, row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?, row.get::<_, i64>(7)?, row.get::<_, i64>(8)?)),
        ).optional()?;
        let Some((run_id, high_water, input, cached, written, output, total, start, end)) = sample
        else {
            return Ok(None);
        };
        if load_run_skills(&self.connection, run_id)? != *skills
            || super::project_context::load(&self.connection, run_id)?.as_ref() != project
        {
            return Ok(None);
        }
        let (input, cached, written, output, total) = (
            count(input)?,
            count(cached)?,
            count(written)?,
            count(output)?,
            count(total)?,
        );
        if input.checked_add(output) != Some(total)
            || cached
                .checked_add(written)
                .is_none_or(|cached| cached > input)
        {
            return Err(PersistenceError::InvalidState {
                reason: "a context usage observation is inconsistent",
            });
        }
        if input == 0 {
            return Ok(None);
        }
        let tail = self.context_budget(session_id, count(high_water)?, through)?;
        // All canonical entries after the observed REQUEST are counted. Output
        // is additionally reserved for ephemeral reasoning/continuation. This can
        // overcount visible output deliberately; cached input is already in input.
        let estimated_tokens = input.saturating_add(output).saturating_add(tail.tokens(0));
        Ok(Some(ContextObservation {
            estimated_tokens,
            usage: RecentProviderUsage {
                input_tokens: input,
                cached_input_tokens: cached,
                cache_write_input_tokens: written,
                output_tokens: output,
                elapsed_milliseconds: count(end)?.checked_sub(count(start)?),
            },
        }))
    }
}

pub(super) fn count(value: i64) -> Result<u64, PersistenceError> {
    u64::try_from(value).map_err(|_| PersistenceError::InvalidState {
        reason: "a context observation counter is negative",
    })
}
