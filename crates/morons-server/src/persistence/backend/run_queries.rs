use rusqlite::params;

use super::{
    Backend,
    records::{load_session, sequence_to_sql},
    repository_import::workspace_summary_at_sequence,
    run_records::{load_required_run, load_scoped_run, transcript_entry_from_row},
    session_events::{active_run_id_at_sequence, load_run_at_sequence, session_event_high_water},
};
use crate::persistence::{
    PersistenceError, Run, RunId, SessionEventCursor, SessionId, TranscriptCursor, TranscriptPage,
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
        let session =
            load_session(&self.connection, session_id)?.ok_or(PersistenceError::SessionNotFound)?;
        let current_entry_high_water = self
            .connection
            .query_row(
                "SELECT entry_high_water FROM session_run_states WHERE session_id = ?1",
                [&session_id.as_bytes()[..]],
                |row| row.get::<_, i64>(0),
            )
            .and_then(|value| {
                u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))
            })?;
        let current_event_high_water = session_event_high_water(&self.connection, session_id)?;
        if cursor.is_some_and(|cursor| cursor.session_id() != session_id) {
            return Err(PersistenceError::InvalidInput {
                reason: "a transcript cursor belongs to another session",
            });
        }
        let (snapshot_entry_sequence, snapshot_event_sequence, after_entry_sequence) = cursor
            .map_or(
                (current_entry_high_water, current_event_high_water, 0),
                |cursor| {
                    (
                        cursor.snapshot_entry_sequence(),
                        cursor.snapshot_event_sequence(),
                        cursor.after_entry_sequence(),
                    )
                },
            );
        if snapshot_entry_sequence > current_entry_high_water
            || snapshot_event_sequence > current_event_high_water
            || after_entry_sequence > snapshot_entry_sequence
        {
            return Err(PersistenceError::InvalidInput {
                reason: "a transcript cursor is outside the available snapshot",
            });
        }
        let snapshot_entry_high_water = self.connection.query_row(
            "SELECT COALESCE(MAX(entry_sequence), 0)
             FROM session_entries
             WHERE session_id = ?1 AND fact_sequence <= ?2",
            params![
                &session_id.as_bytes()[..],
                sequence_to_sql(snapshot_event_sequence)?
            ],
            |row| row.get::<_, i64>(0),
        )?;
        if u64::try_from(snapshot_entry_high_water).ok() != Some(snapshot_entry_sequence) {
            return Err(PersistenceError::InvalidInput {
                reason: "a transcript cursor has inconsistent snapshot high waters",
            });
        }
        let cuts_run_acceptance: bool = self.connection.query_row(
            "SELECT EXISTS (
                 SELECT 1
                 FROM session_entries AS entry
                 INNER JOIN run_accepted_facts AS run ON run.run_id = entry.run_id
                 WHERE entry.session_id = ?1
                   AND entry.entry_sequence <= ?2
                   AND run.fact_sequence > ?3
             )",
            params![
                &session_id.as_bytes()[..],
                sequence_to_sql(snapshot_entry_sequence)?,
                sequence_to_sql(snapshot_event_sequence)?
            ],
            |row| row.get(0),
        )?;
        if cuts_run_acceptance {
            return Err(PersistenceError::InvalidInput {
                reason: "a transcript cursor cuts an atomic run acceptance",
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
               AND fact_sequence <= ?4
             ORDER BY entry_sequence
             LIMIT ?5",
        )?;
        let mut entries = statement
            .query_map(
                params![
                    &session_id.as_bytes()[..],
                    sequence_to_sql(after_entry_sequence)?,
                    sequence_to_sql(snapshot_entry_sequence)?,
                    sequence_to_sql(snapshot_event_sequence)?,
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
                snapshot_event_sequence,
                after_entry_sequence,
            ))
        } else {
            None
        };
        let active_run_id =
            active_run_id_at_sequence(&self.connection, session_id, snapshot_event_sequence)?;
        let mut run_ids = entries
            .iter()
            .map(|entry| entry.run_id())
            .collect::<Vec<_>>();
        if let Some(active_run_id) = active_run_id
            && !run_ids.contains(&active_run_id)
        {
            run_ids.push(active_run_id);
        }
        let runs = run_ids
            .into_iter()
            .map(|run_id| load_run_at_sequence(&self.connection, run_id, snapshot_event_sequence))
            .collect::<Result<Vec<_>, _>>()?;
        let workspace =
            workspace_summary_at_sequence(&self.connection, session_id, snapshot_event_sequence)?;
        Ok(TranscriptPage {
            session,
            workspace,
            entries,
            runs,
            active_run_id,
            next_cursor,
            event_cursor: SessionEventCursor::new(session_id, snapshot_event_sequence),
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
