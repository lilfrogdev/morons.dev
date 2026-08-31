use rusqlite::{OptionalExtension, params};

use super::{
    Backend,
    records::{load_session, sequence_to_sql},
    run_records::{load_required_run, load_scoped_run, transcript_entry_from_row},
};
use crate::persistence::{
    PersistenceError, Run, RunId, SessionId, TranscriptCursor, TranscriptPage,
    run_types::{CONTEXT_POLICY_VERSION, RunContext},
};

impl Backend {
    pub(crate) fn get_run(
        &self,
        session_id: SessionId,
        run_id: RunId,
    ) -> Result<Option<Run>, PersistenceError> {
        if load_session(&self.connection, session_id)?.is_none() {
            return Err(PersistenceError::SessionNotFound);
        }
        load_scoped_run(&self.connection, session_id, run_id)
    }

    pub(crate) fn list_session_transcript(
        &self,
        session_id: SessionId,
        cursor: Option<TranscriptCursor>,
        limit: u16,
    ) -> Result<TranscriptPage, PersistenceError> {
        let current_high_water = self
            .connection
            .query_row(
                "SELECT entry_high_water FROM session_run_states WHERE session_id = ?1",
                [&session_id.as_bytes()[..]],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or(PersistenceError::SessionNotFound)
            .and_then(|value| {
                u64::try_from(value).map_err(|_| PersistenceError::InvalidState {
                    reason: "a session entry high water is invalid",
                })
            })?;
        if cursor.is_some_and(|cursor| cursor.session_id() != session_id) {
            return Err(PersistenceError::InvalidInput {
                reason: "a transcript cursor belongs to another session",
            });
        }
        let (snapshot_entry_sequence, after_entry_sequence) =
            cursor.map_or((current_high_water, 0), |cursor| {
                (
                    cursor.snapshot_entry_sequence(),
                    cursor.after_entry_sequence(),
                )
            });
        if snapshot_entry_sequence > current_high_water
            || after_entry_sequence > snapshot_entry_sequence
        {
            return Err(PersistenceError::InvalidInput {
                reason: "a transcript cursor is outside the available snapshot",
            });
        }

        let mut statement = self.connection.prepare(
            "SELECT
                entry_sequence,
                message_id,
                run_id,
                entry_kind,
                open_code_service,
                model_id,
                text,
                refusal,
                created_at_milliseconds
             FROM session_entries
             WHERE session_id = ?1
               AND entry_sequence > ?2
               AND entry_sequence <= ?3
             ORDER BY entry_sequence
             LIMIT ?4",
        )?;
        let mut entries = statement
            .query_map(
                params![
                    &session_id.as_bytes()[..],
                    sequence_to_sql(after_entry_sequence)?,
                    sequence_to_sql(snapshot_entry_sequence)?,
                    i64::from(limit) + 1,
                ],
                transcript_entry_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let has_more = entries.len() > usize::from(limit);
        if has_more {
            entries.pop();
        }
        let next_cursor = if has_more {
            let after_entry_sequence = entries
                .last()
                .ok_or(PersistenceError::InvalidState {
                    reason: "a transcript page lost its continuation entry",
                })?
                .entry_sequence();
            Some(TranscriptCursor::new(
                session_id,
                snapshot_entry_sequence,
                after_entry_sequence,
            ))
        } else {
            None
        };
        Ok(TranscriptPage {
            entries,
            next_cursor,
        })
    }

    pub(crate) fn load_run_context(&self, run_id: RunId) -> Result<RunContext, PersistenceError> {
        let run = load_required_run(&self.connection, run_id)?;
        if run.context_policy_version != CONTEXT_POLICY_VERSION {
            return Err(PersistenceError::InvalidState {
                reason: "a run uses an unsupported context policy version",
            });
        }
        let mut statement = self.connection.prepare(
            "SELECT
                entry_sequence,
                message_id,
                run_id,
                entry_kind,
                open_code_service,
                model_id,
                text,
                refusal,
                created_at_milliseconds
             FROM session_entries
             WHERE session_id = ?1 AND entry_sequence <= ?2
             ORDER BY entry_sequence",
        )?;
        let entries = statement
            .query_map(
                params![
                    &run.session_id.as_bytes()[..],
                    sequence_to_sql(run.source_entry_high_water)?
                ],
                transcript_entry_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        if entries.is_empty()
            || entries.last().map(|entry| entry.entry_sequence())
                != Some(run.source_entry_high_water)
        {
            return Err(PersistenceError::InvalidState {
                reason: "a run context source high water is not present",
            });
        }
        Ok(RunContext { run, entries })
    }
}
