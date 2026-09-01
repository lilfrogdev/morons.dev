use rusqlite::{OptionalExtension, params};

use super::{
    Backend,
    records::{load_session, sequence_to_sql},
    repository_import::workspace_summary_at_sequence,
    run_records::{load_required_run, load_scoped_run, transcript_entry_from_row},
    session_events::{active_run_id_at_sequence, load_run_at_sequence, session_event_high_water},
};
use crate::persistence::{
    PersistenceError, Run, RunId, SessionEventCursor, SessionId, TranscriptCursor, TranscriptEntry,
    TranscriptPage,
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
                entry.entry_sequence,
                entry.message_id,
                entry.run_id,
                entry.entry_kind,
                entry.open_code_service,
                entry.model_id,
                entry.text,
                entry.refusal,
                entry.created_at_milliseconds,
                entry.assistant_phase,
                entry.tool_call_id,
                call.operation_id,
                call.provider_operation_id,
                call.input_payload,
                result.result_payload
             FROM session_entries AS entry
             LEFT JOIN tool_calls AS call ON call.call_id = entry.tool_call_id
             LEFT JOIN tool_operation_facts AS result
               ON result.call_id = entry.tool_call_id AND result.fact_kind BETWEEN 3 AND 6
             WHERE entry.session_id = ?1
               AND entry.entry_sequence > ?2
               AND entry.entry_sequence <= ?3
               AND entry.fact_sequence <= ?4
             ORDER BY entry.entry_sequence
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
        let current_entry_high_water = self.connection.query_row(
            "SELECT entry_high_water FROM session_run_states WHERE session_id = ?1",
            [&run.session_id.as_bytes()[..]],
            |row| row.get::<_, i64>(0),
        )?;
        let current_entry_high_water = u64::try_from(current_entry_high_water).map_err(|_| {
            PersistenceError::InvalidState {
                reason: "the current run entry high water is invalid",
            }
        })?;
        if current_entry_high_water < run.source_entry_high_water {
            return Err(PersistenceError::InvalidState {
                reason: "the current run context precedes its accepted source",
            });
        }
        let mut statement = self.connection.prepare(
            "SELECT
                entry.entry_sequence,
                entry.message_id,
                entry.run_id,
                entry.entry_kind,
                entry.open_code_service,
                entry.model_id,
                entry.text,
                entry.refusal,
                entry.created_at_milliseconds,
                entry.assistant_phase,
                entry.tool_call_id,
                call.operation_id,
                call.provider_operation_id,
                call.input_payload,
                result.result_payload
             FROM session_entries AS entry
             LEFT JOIN tool_calls AS call ON call.call_id = entry.tool_call_id
             LEFT JOIN tool_operation_facts AS result
               ON result.call_id = entry.tool_call_id AND result.fact_kind BETWEEN 3 AND 6
             WHERE entry.session_id = ?1 AND entry.entry_sequence <= ?2
             ORDER BY entry.entry_sequence",
        )?;
        let entries = statement
            .query_map(
                params![
                    &run.session_id.as_bytes()[..],
                    sequence_to_sql(current_entry_high_water)?
                ],
                transcript_entry_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        if entries.is_empty()
            || entries.last().map(TranscriptEntry::entry_sequence) != Some(current_entry_high_water)
        {
            return Err(PersistenceError::InvalidState {
                reason: "a run context high water is not present",
            });
        }
        let context_bytes = entries
            .iter()
            .try_fold(0_u64, |total, entry| {
                let bytes = match entry {
                    TranscriptEntry::UserMessage { text, .. }
                    | TranscriptEntry::AssistantMessage { text, .. } => text.len(),
                    TranscriptEntry::ToolCall { input, .. } => {
                        serde_json::to_vec(input).ok()?.len()
                    }
                    TranscriptEntry::ToolResult { result, .. } => {
                        result.provider_output().ok()?.len()
                    }
                };
                total.checked_add(bytes as u64)
            })
            .ok_or(PersistenceError::ResourceLimit {
                resource: crate::persistence::PersistenceResourceLimit::Context,
            })?;
        let estimated_input_tokens =
            crate::persistence::run_types::conservative_input_token_estimate(
                context_bytes,
                entries.len() as u64,
            )
            .filter(|estimate| *estimate <= run.maximum_input_tokens)
            .ok_or(PersistenceError::ResourceLimit {
                resource: crate::persistence::PersistenceResourceLimit::Context,
            })?;
        let workspace_id = self
            .connection
            .query_row(
                "SELECT session.workspace_id
                 FROM session_created_facts AS session
                 JOIN repository_import_requests AS repository
                   ON repository.session_id = session.session_id AND repository.state = 2
                 WHERE session.session_id = ?1",
                [&run.session_id.as_bytes()[..]],
                |row| row.get::<_, [u8; 16]>(0),
            )
            .optional()?;
        let valid_tool_versions =
            matches!((run.tool_catalog_version, run.tool_limits_version), (0, 0))
                || (run.tool_catalog_version == crate::tools::TOOL_CATALOG_VERSION
                    && run.tool_limits_version == crate::tools::TOOL_LIMITS_VERSION
                    && workspace_id.is_some());
        if !valid_tool_versions {
            return Err(PersistenceError::InvalidState {
                reason: "the run tool catalog conflicts with its ready workspace",
            });
        }
        Ok(RunContext {
            run,
            entries,
            current_entry_high_water,
            estimated_input_tokens,
            workspace_id,
        })
    }
}
