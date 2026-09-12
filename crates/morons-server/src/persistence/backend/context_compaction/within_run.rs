use super::super::{
    Backend,
    context_budget::MAX_COMPACTION_SUMMARY_BYTES,
    context_execution::{self, ExecutionPolicy},
    records::sequence_to_sql,
};
use crate::persistence::{PersistenceError, Run};
use rusqlite::params;

impl Backend {
    pub(super) fn select_within_run_cut(
        &self,
        run: &Run,
        covered: u64,
        through: u64,
        instructions: usize,
    ) -> Result<Option<u64>, PersistenceError> {
        if context_execution::policy(&self.connection, run.id)? != ExecutionPolicy::NativeUsage {
            return Ok(None);
        }
        let user = self.context_budget(
            run.session_id,
            run.source_entry_high_water - 1,
            run.source_entry_high_water,
        )?;
        if user.images != 0 {
            return Ok(None);
        }
        let mut statement = self.connection.prepare(
            "SELECT MAX(entry.entry_sequence) FROM session_entries AS entry
             JOIN tool_calls AS call ON call.call_id = entry.tool_call_id
             WHERE call.run_id = ?1 AND entry.entry_kind = 4
             GROUP BY call.provider_operation_id ORDER BY MAX(entry.entry_sequence) ASC LIMIT 32",
        )?;
        let rows = statement.query_map([&run.id.as_bytes()[..]], |r| r.get::<_, i64>(0))?;
        for row in rows {
            let cut = u64::try_from(row?).map_err(|_| invalid())?;
            if cut <= covered
                || cut >= through
                || !context_execution::complete_cut(&self.connection, run.id, cut)?
            {
                continue;
            }
            let mut tail = self.context_budget(run.session_id, cut, through)?;
            // At least the most recent completed call/result batch stays canonical.
            if tail.entries < 2 {
                continue;
            }
            tail.include(&user);
            if tail.fits(
                run.maximum_input_tokens,
                instructions + MAX_COMPACTION_SUMMARY_BYTES,
            ) && !self.compaction_prefix_was_attempted(run.session_id, cut)?
            {
                return Ok(Some(cut));
            }
        }
        Ok(None)
    }
}

pub(in crate::persistence) fn source_allowed(
    connection: &rusqlite::Connection,
    run: crate::persistence::RunId,
    cut: u64,
    prepared_sequence: Option<i64>,
) -> Result<bool, PersistenceError> {
    let (session, user, initial): ([u8;16], [u8;16], i64) = connection.query_row("SELECT session_id, user_message_id, source_entry_high_water FROM run_accepted_facts WHERE run_id = ?1", [&run.as_bytes()[..]], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    if cut < u64::try_from(initial).map_err(|_| invalid())? {
        return Ok(true);
    }
    if context_execution::policy(connection, run)? != ExecutionPolicy::NativeUsage {
        return Ok(false);
    }
    let metadata_valid: bool = connection.query_row(
        "SELECT ?2 < state.entry_high_water
            AND NOT EXISTS (SELECT 1 FROM image_attachments WHERE user_message_id = ?3)
            AND EXISTS (SELECT 1 FROM session_entries WHERE run_id = ?1 AND entry_sequence = ?2 AND entry_kind = 4 AND fact_sequence < ?5)
         FROM (SELECT MAX(entry_sequence) AS entry_high_water FROM session_entries WHERE session_id = ?4 AND fact_sequence < ?5) AS state",
        params![&run.as_bytes()[..], sequence_to_sql(cut)?, &user[..], &session[..], prepared_sequence.unwrap_or(i64::MAX)], |r| r.get(0))?;
    Ok(metadata_valid && context_execution::complete_cut(connection, run, cut)?)
}

fn invalid() -> PersistenceError {
    PersistenceError::InvalidState {
        reason: "a within-run compaction cut is invalid",
    }
}
