use rusqlite::{OptionalExtension as _, params};

use super::{
    Backend,
    context_usage::{ContextModel, count},
    run_queries::{load_latest_checkpoint, load_run_skills},
};
use crate::persistence::{
    PersistenceError, RunId, RunModelSelection, SessionContextStatus, SessionId,
};

impl Backend {
    /// Status must not perform inference preparation, summary projection or image I/O.
    pub(crate) fn session_context_status(
        &self,
        session_id: SessionId,
        selection: &RunModelSelection,
    ) -> Result<SessionContextStatus, PersistenceError> {
        let exists: bool = self.connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM sessions WHERE session_id = ?1)",
            [&session_id.as_bytes()[..]],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(PersistenceError::SessionNotFound);
        }
        self.ensure_context_integrity()?;
        let through = self
            .connection
            .query_row(
                "SELECT entry_high_water FROM session_run_states WHERE session_id = ?1",
                [&session_id.as_bytes()[..]],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map(count)
            .transpose()?
            .unwrap_or(0);
        let latest_run = self.connection.query_row(
            "SELECT run_id FROM run_accepted_facts WHERE session_id = ?1 ORDER BY fact_sequence DESC LIMIT 1",
            [&session_id.as_bytes()[..]], |row| row.get::<_, [u8;16]>(0),
        ).optional()?;
        let skills = match latest_run {
            Some(id) => load_run_skills(&self.connection, RunId::from_bytes(id))?,
            None => crate::skills::RunSkillContext::default(),
        };
        let checkpoint = load_latest_checkpoint(&self.connection, session_id, through)?;
        let extra_bytes = skills
            .context_bytes()
            .ok_or(PersistenceError::InvalidState {
                reason: "a context skill snapshot has invalid bounds",
            })?
            .saturating_add(
                checkpoint
                    .as_ref()
                    .map_or(0, |checkpoint| checkpoint.summary.len()),
            );
        let mut budget = self.context_budget(
            session_id,
            checkpoint
                .as_ref()
                .map_or(0, |checkpoint| checkpoint.source_entry_high_water),
            through,
        )?;
        let observation = self.observe_context_usage(
            session_id,
            ContextModel {
                service: selection.service,
                model_id: &selection.model_id,
                protocol_revision: selection.protocol_revision,
            },
            checkpoint.as_ref(),
            through,
            &skills,
        )?;
        budget.observed_input_tokens = observation
            .as_ref()
            .map(|observation| observation.estimated_tokens);
        let (completed_compactions, last_compaction_milliseconds) =
            self.compaction_metrics(session_id, checkpoint.as_ref())?;
        Ok(SessionContextStatus {
            estimated_input_tokens: u32::try_from(budget.estimated_tokens(extra_bytes))
                .unwrap_or(u32::MAX),
            conservative_input_tokens: u32::try_from(budget.tokens(extra_bytes))
                .unwrap_or(u32::MAX),
            estimate_uses_provider_usage: observation.is_some(),
            latest_provider_usage: observation.map(|observation| observation.usage),
            completed_compactions,
            last_compaction_milliseconds,
            maximum_input_tokens: selection.maximum_input_tokens,
            maximum_output_tokens: selection.maximum_output_tokens,
            compaction_threshold_tokens: selection.maximum_input_tokens.saturating_mul(7) / 10,
            checkpoint_source_entry_high_water: checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.source_entry_high_water),
            checkpoint_estimated_summary_tokens: checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.estimated_summary_tokens),
        })
    }

    fn compaction_metrics(
        &self,
        session_id: SessionId,
        checkpoint: Option<&crate::persistence::ContextCheckpoint>,
    ) -> Result<(u64, Option<u64>), PersistenceError> {
        let Some(checkpoint) = checkpoint else {
            return Ok((0, None));
        };
        // Use the session checkpoint index and unique operation/checkpoint key,
        // not a scan of every session's compaction operations.
        let completed: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM context_checkpoints AS checkpoint
             CROSS JOIN compaction_operations AS operation ON operation.checkpoint_id = checkpoint.checkpoint_id
             WHERE checkpoint.session_id = ?1 AND operation.state = 3",
            [&session_id.as_bytes()[..]], |row| row.get(0),
        )?;
        let elapsed: Option<i64> = self.connection.query_row(
            "SELECT updated_at_milliseconds - prepared_at_milliseconds FROM compaction_operations
             WHERE checkpoint_id = ?1 AND state = 3",
            params![&checkpoint.id.as_bytes()[..]], |row| row.get(0),
        ).optional()?;
        Ok((count(completed)?, elapsed.map(count).transpose()?))
    }
}
