use rusqlite::{Connection, OptionalExtension, params};

use super::{
    Backend,
    records::{load_session, nonnegative_integer_from_row, sequence_to_sql},
    repository_import::workspace_summary_for_event,
    run_records::{positive_u16_from_row, positive_u32_from_row, transcript_entry_from_row},
};
use crate::persistence::{
    MessageId, PersistenceError, Run, RunFailureKind, RunId, RunOpenCodeService, RunState,
    SessionEvent, SessionEventCursor, SessionEventPage, SessionEventPayload, SessionId,
    TranscriptEntry,
};

const EVENT_USER_MESSAGE: i64 = 2;
const EVENT_RUN_ACCEPTED: i64 = 3;
const EVENT_RUN_ACTIVE: i64 = 4;
const EVENT_CANCELLATION_REQUESTED: i64 = 5;
const EVENT_ASSISTANT_MESSAGE: i64 = 6;
const EVENT_RUN_SUCCEEDED: i64 = 7;
const EVENT_RUN_FAILED: i64 = 8;
const EVENT_RUN_CANCELLED: i64 = 9;
const EVENT_RUN_INTERRUPTED: i64 = 10;
const EVENT_WORKSPACE_CHANGED: i64 = 11;
const EVENT_TOOL_CALL: i64 = 12;
const EVENT_TOOL_RESULT: i64 = 13;
const EVENT_RUN_UNCERTAIN: i64 = 14;
const EVENT_TOOL_UNCERTAINTY_CHANGED: i64 = 15;
const EVENT_LOCAL_COMMAND_CHANGED: i64 = 16;
const EVENT_LOCAL_COMMAND_ENTRY: i64 = 17;

impl Backend {
    pub(crate) fn delivery_event_high_water(&self) -> Result<u64, PersistenceError> {
        let value = self.connection.query_row(
            "SELECT COALESCE(MAX(event_sequence), 0) FROM delivery_events",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        u64::try_from(value).map_err(|_| PersistenceError::InvalidState {
            reason: "the delivery event high water is invalid",
        })
    }

    pub(crate) fn read_session_events(
        &self,
        session_id: SessionId,
        cursor: SessionEventCursor,
        limit: u16,
    ) -> Result<SessionEventPage, PersistenceError> {
        if cursor.session_id() != session_id {
            return Err(PersistenceError::InvalidInput {
                reason: "a session event cursor belongs to another session",
            });
        }
        if load_session(&self.connection, session_id)?.is_none() {
            return Err(PersistenceError::SessionNotFound);
        }
        let high_water = session_event_high_water(&self.connection, session_id)?;
        if cursor.sequence() > high_water {
            return Err(PersistenceError::InvalidInput {
                reason: "a session event cursor is ahead of the durable event stream",
            });
        }

        let mut statement = self.connection.prepare(
            "SELECT event_sequence, event_id, event_kind
             FROM delivery_events
             WHERE session_id = ?1
               AND event_sequence > ?2
               AND event_sequence <= ?3
               AND event_kind BETWEEN 2 AND 17
               AND payload_version = 1
             ORDER BY event_sequence
             LIMIT ?4",
        )?;
        let records = statement
            .query_map(
                params![
                    &session_id.as_bytes()[..],
                    sequence_to_sql(cursor.sequence())?,
                    sequence_to_sql(high_water)?,
                    i64::from(limit),
                ],
                |row| {
                    Ok((
                        nonnegative_integer_from_row(row, 0)?,
                        row.get::<_, [u8; 16]>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);

        let events = records
            .into_iter()
            .map(|(sequence, event_id, event_kind)| {
                let payload = match event_kind {
                    EVENT_USER_MESSAGE
                    | EVENT_ASSISTANT_MESSAGE
                    | EVENT_TOOL_CALL
                    | EVENT_TOOL_RESULT => SessionEventPayload::TranscriptEntry(
                        load_entry_for_event(&self.connection, session_id, &event_id, event_kind)?,
                    ),
                    EVENT_LOCAL_COMMAND_CHANGED => {
                        let command_id = self
                            .connection
                            .query_row(
                                "SELECT command_id FROM local_commands
                                 WHERE session_id = ?1 AND accepted_event_id = ?2",
                                params![&session_id.as_bytes()[..], &event_id[..]],
                                |row| row.get::<_, [u8; 16]>(0),
                            )
                            .optional()?
                            .ok_or(PersistenceError::InvalidState {
                                reason: "a local command event is missing its accepted command",
                            })?;
                        SessionEventPayload::LocalCommandChanged {
                            command_id: crate::persistence::LocalCommandId::from_bytes(command_id),
                            active: true,
                        }
                    }
                    EVENT_LOCAL_COMMAND_ENTRY => SessionEventPayload::TranscriptEntry(
                        self.load_local_command_entry_for_event(session_id, &event_id)?,
                    ),
                    EVENT_RUN_ACCEPTED
                    | EVENT_RUN_ACTIVE
                    | EVENT_CANCELLATION_REQUESTED
                    | EVENT_RUN_SUCCEEDED
                    | EVENT_RUN_FAILED
                    | EVENT_RUN_CANCELLED
                    | EVENT_RUN_INTERRUPTED
                    | EVENT_RUN_UNCERTAIN => {
                        let run_id =
                            load_event_run_id(&self.connection, session_id, &event_id, event_kind)?;
                        let run = load_run_at_sequence(&self.connection, run_id, sequence)?;
                        validate_event_run(event_kind, &run)?;
                        SessionEventPayload::RunChanged(run)
                    }
                    EVENT_WORKSPACE_CHANGED | EVENT_TOOL_UNCERTAINTY_CHANGED => {
                        SessionEventPayload::WorkspaceChanged(workspace_summary_for_event(
                            &self.connection,
                            session_id,
                            &event_id,
                            sequence,
                        )?)
                    }
                    _ => {
                        return Err(PersistenceError::InvalidState {
                            reason: "a session delivery event has an unsupported kind",
                        });
                    }
                };
                Ok(SessionEvent {
                    cursor: SessionEventCursor::new(session_id, sequence),
                    payload,
                })
            })
            .collect::<Result<Vec<_>, PersistenceError>>()?;
        Ok(SessionEventPage {
            events,
            high_water: SessionEventCursor::new(session_id, high_water),
        })
    }
}

pub(super) fn session_event_high_water(
    connection: &Connection,
    session_id: SessionId,
) -> Result<u64, PersistenceError> {
    let value = connection.query_row(
        "SELECT COALESCE(MAX(event_sequence), 0)
         FROM delivery_events
         WHERE session_id = ?1",
        [&session_id.as_bytes()[..]],
        |row| row.get::<_, i64>(0),
    )?;
    u64::try_from(value).map_err(|_| PersistenceError::InvalidState {
        reason: "a session event high water is invalid",
    })
}

pub(super) fn load_run_at_sequence(
    connection: &Connection,
    run_id: RunId,
    event_sequence: u64,
) -> Result<Run, PersistenceError> {
    let mut run = connection
        .query_row(
            "SELECT
                session_id,
                user_message_id,
                open_code_service,
                model_id,
                protocol_revision,
                credential_generation,
                context_policy_version,
                source_entry_high_water,
                estimated_input_tokens,
                maximum_input_tokens,
                maximum_output_tokens,
                accepted_at_milliseconds,
                tool_catalog_version,
                tool_limits_version,
                execution_image_generation
             FROM run_accepted_facts
             WHERE run_id = ?1 AND fact_sequence <= ?2",
            params![&run_id.as_bytes()[..], sequence_to_sql(event_sequence)?],
            |row| {
                let accepted_at_milliseconds = nonnegative_integer_from_row(row, 11)?;
                Ok(Run {
                    id: run_id,
                    session_id: SessionId::from_bytes(row.get(0)?),
                    user_message_id: MessageId::from_bytes(row.get(1)?),
                    service: RunOpenCodeService::from_record(row.get(2)?)?,
                    model_id: row.get(3)?,
                    protocol_revision: positive_u16_from_row(row, 4)?,
                    credential_generation: nonnegative_integer_from_row(row, 5)?,
                    context_policy_version: positive_u16_from_row(row, 6)?,
                    tool_catalog_version: super::run_records::nonnegative_u16_from_row(row, 12)?,
                    tool_limits_version: super::run_records::nonnegative_u16_from_row(row, 13)?,
                    execution_image_generation: row.get(14)?,
                    source_entry_high_water: nonnegative_integer_from_row(row, 7)?,
                    estimated_input_tokens: positive_u32_from_row(row, 8)?,
                    maximum_input_tokens: positive_u32_from_row(row, 9)?,
                    maximum_output_tokens: positive_u32_from_row(row, 10)?,
                    provider_turns: 0,
                    tool_calls: 0,
                    tool_mutations: 0,
                    tool_result_bytes: 0,
                    state: RunState::Accepted,
                    cancellation_requested: false,
                    failure: None,
                    accepted_at_milliseconds,
                    updated_at_milliseconds: accepted_at_milliseconds,
                })
            },
        )
        .optional()?
        .ok_or(PersistenceError::InvalidState {
            reason: "a session event references a run outside its snapshot",
        })?;

    let state = connection
        .query_row(
            "SELECT state, failure_kind, created_at_milliseconds, fact_sequence
             FROM run_state_facts
             WHERE run_id = ?1 AND fact_sequence <= ?2
             ORDER BY fact_sequence DESC
             LIMIT 1",
            params![&run_id.as_bytes()[..], sequence_to_sql(event_sequence)?],
            |row| {
                let state = RunState::from_record(row.get(0)?)?;
                let failure = row
                    .get::<_, Option<i64>>(1)?
                    .map(RunFailureKind::from_record)
                    .transpose()?;
                Ok((
                    state,
                    failure,
                    nonnegative_integer_from_row(row, 2)?,
                    nonnegative_integer_from_row(row, 3)?,
                ))
            },
        )
        .optional()?;
    let mut updated_sequence = 0;
    if let Some((state, failure, updated_at, sequence)) = state {
        if (state == RunState::Failed) != failure.is_some() {
            return Err(PersistenceError::InvalidState {
                reason: "a session event run has an invalid failure classification",
            });
        }
        run.state = state;
        run.failure = failure;
        run.updated_at_milliseconds = updated_at;
        updated_sequence = sequence;
    }

    let cancellation = connection
        .query_row(
            "SELECT accepted_at_milliseconds, fact_sequence
             FROM run_cancellation_requests
             WHERE run_id = ?1 AND intent_applied = 1 AND fact_sequence <= ?2
             ORDER BY fact_sequence DESC
             LIMIT 1",
            params![&run_id.as_bytes()[..], sequence_to_sql(event_sequence)?],
            |row| {
                Ok((
                    nonnegative_integer_from_row(row, 0)?,
                    nonnegative_integer_from_row(row, 1)?,
                ))
            },
        )
        .optional()?;
    if let Some((updated_at, sequence)) = cancellation {
        run.cancellation_requested = true;
        if sequence > updated_sequence {
            run.updated_at_milliseconds = updated_at;
        }
    }
    Ok(run)
}

pub(super) fn active_run_id_at_sequence(
    connection: &Connection,
    session_id: SessionId,
    event_sequence: u64,
) -> Result<Option<RunId>, PersistenceError> {
    let mut statement = connection.prepare(
        "SELECT accepted.run_id
         FROM run_accepted_facts AS accepted
         WHERE accepted.session_id = ?1
           AND accepted.fact_sequence <= ?2
           AND NOT EXISTS (
               SELECT 1
               FROM run_state_facts AS state
               WHERE state.run_id = accepted.run_id
                 AND state.fact_sequence <= ?2
                 AND state.state BETWEEN 3 AND 7
           )
         ORDER BY accepted.fact_sequence DESC
         LIMIT 2",
    )?;
    let run_ids = statement
        .query_map(
            params![&session_id.as_bytes()[..], sequence_to_sql(event_sequence)?],
            |row| row.get::<_, [u8; 16]>(0).map(RunId::from_bytes),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    match run_ids.as_slice() {
        [] => Ok(None),
        [run_id] => Ok(Some(*run_id)),
        _ => Err(PersistenceError::InvalidState {
            reason: "a session snapshot has multiple nonterminal runs",
        }),
    }
}

fn load_entry_for_event(
    connection: &Connection,
    session_id: SessionId,
    event_id: &[u8; 16],
    event_kind: i64,
) -> Result<TranscriptEntry, PersistenceError> {
    let entry = connection
        .query_row(
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
             WHERE entry.session_id = ?1 AND entry.delivery_event_id = ?2",
            params![&session_id.as_bytes()[..], &event_id[..]],
            transcript_entry_from_row,
        )
        .optional()?
        .ok_or(PersistenceError::InvalidState {
            reason: "a transcript delivery event is missing its canonical entry",
        })?;
    let valid = matches!(
        (&entry, event_kind),
        (TranscriptEntry::UserMessage { .. }, EVENT_USER_MESSAGE)
            | (
                TranscriptEntry::AssistantMessage { .. },
                EVENT_ASSISTANT_MESSAGE
            )
            | (TranscriptEntry::ToolCall { .. }, EVENT_TOOL_CALL)
            | (TranscriptEntry::ToolResult { .. }, EVENT_TOOL_RESULT)
    );
    if !valid {
        return Err(PersistenceError::InvalidState {
            reason: "a transcript delivery event conflicts with its entry kind",
        });
    }
    Ok(entry)
}

fn load_event_run_id(
    connection: &Connection,
    session_id: SessionId,
    event_id: &[u8; 16],
    event_kind: i64,
) -> Result<RunId, PersistenceError> {
    let query = match event_kind {
        EVENT_RUN_ACCEPTED => {
            "SELECT run_id FROM run_accepted_facts WHERE session_id = ?1 AND delivery_event_id = ?2"
        }
        EVENT_CANCELLATION_REQUESTED => {
            "SELECT run_id FROM run_cancellation_requests WHERE session_id = ?1 AND delivery_event_id = ?2 AND intent_applied = 1"
        }
        EVENT_RUN_ACTIVE
        | EVENT_RUN_SUCCEEDED
        | EVENT_RUN_FAILED
        | EVENT_RUN_CANCELLED
        | EVENT_RUN_INTERRUPTED
        | EVENT_RUN_UNCERTAIN => {
            "SELECT run_id FROM run_state_facts WHERE session_id = ?1 AND delivery_event_id = ?2"
        }
        _ => {
            return Err(PersistenceError::InvalidState {
                reason: "a run delivery event has an unsupported kind",
            });
        }
    };
    connection
        .query_row(
            query,
            params![&session_id.as_bytes()[..], &event_id[..]],
            |row| row.get::<_, [u8; 16]>(0).map(RunId::from_bytes),
        )
        .optional()?
        .ok_or(PersistenceError::InvalidState {
            reason: "a run delivery event is missing its canonical fact",
        })
}

fn validate_event_run(event_kind: i64, run: &Run) -> Result<(), PersistenceError> {
    let valid = match event_kind {
        EVENT_RUN_ACCEPTED => run.state == RunState::Accepted && !run.cancellation_requested,
        EVENT_RUN_ACTIVE => run.state == RunState::Active,
        EVENT_CANCELLATION_REQUESTED => run.cancellation_requested && !run.state.is_terminal(),
        EVENT_RUN_SUCCEEDED => run.state == RunState::Succeeded,
        EVENT_RUN_FAILED => run.state == RunState::Failed,
        EVENT_RUN_CANCELLED => run.state == RunState::Cancelled,
        EVENT_RUN_INTERRUPTED => run.state == RunState::Interrupted,
        EVENT_RUN_UNCERTAIN => run.state == RunState::Uncertain,
        _ => false,
    };
    if !valid {
        return Err(PersistenceError::InvalidState {
            reason: "a run delivery event conflicts with its run state",
        });
    }
    Ok(())
}
