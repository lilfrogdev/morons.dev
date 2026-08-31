use rusqlite::{Connection, OptionalExtension};

use super::records::nonnegative_integer_from_row;
use crate::persistence::{
    AcceptedRun, MessageId, PersistenceError, Run, RunFailureKind, RunId, RunOpenCodeService,
    RunState, SessionId, TranscriptEntry, types::REQUEST_FINGERPRINT_BYTES,
};

pub(super) const RUN_STATE_ACTIVE: i64 = 2;
pub(super) const RUN_STATE_SUCCEEDED: i64 = 3;
pub(super) const RUN_STATE_FAILED: i64 = 4;
pub(super) const RUN_STATE_CANCELLED: i64 = 5;
pub(super) const RUN_STATE_INTERRUPTED: i64 = 6;

pub(super) const PROVIDER_FACT_PREPARED: i64 = 1;
pub(super) const PROVIDER_FACT_DISPATCHED: i64 = 2;
pub(super) const PROVIDER_FACT_FAILED: i64 = 4;
pub(super) const PROVIDER_FACT_UNCERTAIN: i64 = 5;
pub(super) const PROVIDER_FACT_ABANDONED: i64 = 6;

pub(super) const EVENT_USER_MESSAGE: i64 = 2;
pub(super) const EVENT_RUN_ACCEPTED: i64 = 3;
pub(super) const EVENT_RUN_ACTIVE: i64 = 4;
pub(super) const EVENT_CANCELLATION_REQUESTED: i64 = 5;
pub(super) const EVENT_ASSISTANT_MESSAGE: i64 = 6;
pub(super) const EVENT_RUN_SUCCEEDED: i64 = 7;
pub(super) const EVENT_RUN_FAILED: i64 = 8;
pub(super) const EVENT_RUN_CANCELLED: i64 = 9;
pub(super) const EVENT_RUN_INTERRUPTED: i64 = 10;

pub(super) const AUDIT_INPUT_ACCEPTED: i64 = 1;
pub(super) const AUDIT_RUN_ACTIVE: i64 = 2;
pub(super) const AUDIT_PROVIDER_PREPARED: i64 = 3;
pub(super) const AUDIT_PROVIDER_DISPATCHED: i64 = 4;
pub(super) const AUDIT_PROVIDER_COMPLETED: i64 = 5;
pub(super) const AUDIT_PROVIDER_FAILED: i64 = 6;
pub(super) const AUDIT_CANCELLATION_REQUESTED: i64 = 7;
pub(super) const AUDIT_RUN_SUCCEEDED: i64 = 8;
pub(super) const AUDIT_RUN_FAILED: i64 = 9;
pub(super) const AUDIT_RUN_STOPPED: i64 = 10;

pub(super) struct RunInputRequest {
    pub(super) fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
    pub(super) session_id: SessionId,
    pub(super) run_id: RunId,
    pub(super) user_message_id: MessageId,
}

pub(super) fn load_run_input_request(
    connection: &Connection,
    request_id: crate::persistence::MutationRequestId,
) -> Result<Option<RunInputRequest>, PersistenceError> {
    connection
        .query_row(
            "SELECT operation_fingerprint, session_id, run_id, user_message_id
             FROM run_input_requests
             WHERE request_id = ?1",
            [&request_id.as_bytes()[..]],
            |row| {
                Ok(RunInputRequest {
                    fingerprint: row.get(0)?,
                    session_id: SessionId::from_bytes(row.get(1)?),
                    run_id: RunId::from_bytes(row.get(2)?),
                    user_message_id: MessageId::from_bytes(row.get(3)?),
                })
            },
        )
        .optional()
        .map_err(PersistenceError::from)
}

pub(super) fn accepted_run_from_request(
    connection: &Connection,
    request: &RunInputRequest,
    newly_accepted: bool,
) -> Result<AcceptedRun, PersistenceError> {
    let run = connection
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
                accepted_at_milliseconds
             FROM run_accepted_facts
             WHERE run_id = ?1",
            [&request.run_id.as_bytes()[..]],
            |row| {
                let accepted_at_milliseconds = nonnegative_integer_from_row(row, 11)?;
                Ok(Run {
                    id: request.run_id,
                    session_id: SessionId::from_bytes(row.get(0)?),
                    user_message_id: MessageId::from_bytes(row.get(1)?),
                    service: RunOpenCodeService::from_record(row.get(2)?)?,
                    model_id: row.get(3)?,
                    protocol_revision: positive_u16_from_row(row, 4)?,
                    credential_generation: nonnegative_integer_from_row(row, 5)?,
                    context_policy_version: positive_u16_from_row(row, 6)?,
                    source_entry_high_water: nonnegative_integer_from_row(row, 7)?,
                    estimated_input_tokens: positive_u32_from_row(row, 8)?,
                    maximum_input_tokens: positive_u32_from_row(row, 9)?,
                    maximum_output_tokens: positive_u32_from_row(row, 10)?,
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
            reason: "an accepted run input is missing its canonical run fact",
        })?;
    if run.session_id != request.session_id || run.user_message_id != request.user_message_id {
        return Err(PersistenceError::InvalidState {
            reason: "an accepted run input conflicts with its canonical run fact",
        });
    }
    Ok(AcceptedRun {
        user_message_id: request.user_message_id,
        run,
        newly_accepted,
    })
}

pub(super) fn load_run(
    connection: &Connection,
    run_id: RunId,
) -> Result<Option<Run>, PersistenceError> {
    connection
        .query_row(
            "SELECT
                run_id,
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
                state,
                cancellation_requested,
                failure_kind,
                accepted_at_milliseconds,
                updated_at_milliseconds
             FROM runs
             WHERE run_id = ?1",
            [&run_id.as_bytes()[..]],
            run_from_row,
        )
        .optional()
        .map_err(PersistenceError::from)
}

pub(super) fn load_scoped_run(
    connection: &Connection,
    session_id: SessionId,
    run_id: RunId,
) -> Result<Option<Run>, PersistenceError> {
    let run = load_run(connection, run_id)?;
    Ok(run.filter(|run| run.session_id == session_id))
}

pub(super) fn load_required_run(
    connection: &Connection,
    run_id: RunId,
) -> Result<Run, PersistenceError> {
    load_run(connection, run_id)?.ok_or(PersistenceError::InvalidState {
        reason: "a canonical run is missing its current-state projection",
    })
}

pub(super) fn run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Run> {
    let protocol_revision = positive_u16_from_row(row, 5)?;
    let context_policy_version = positive_u16_from_row(row, 7)?;
    let estimated_input_tokens = positive_u32_from_row(row, 9)?;
    let maximum_input_tokens = positive_u32_from_row(row, 10)?;
    let maximum_output_tokens = positive_u32_from_row(row, 11)?;
    let state = RunState::from_record(row.get(12)?)?;
    let failure = row
        .get::<_, Option<i64>>(14)?
        .map(RunFailureKind::from_record)
        .transpose()?;
    if (state == RunState::Failed) != failure.is_some() {
        return Err(rusqlite::Error::InvalidColumnType(
            14,
            "failure_kind".to_owned(),
            rusqlite::types::Type::Integer,
        ));
    }
    Ok(Run {
        id: RunId::from_bytes(row.get(0)?),
        session_id: SessionId::from_bytes(row.get(1)?),
        user_message_id: MessageId::from_bytes(row.get(2)?),
        service: RunOpenCodeService::from_record(row.get(3)?)?,
        model_id: row.get(4)?,
        protocol_revision,
        credential_generation: nonnegative_integer_from_row(row, 6)?,
        context_policy_version,
        source_entry_high_water: nonnegative_integer_from_row(row, 8)?,
        estimated_input_tokens,
        maximum_input_tokens,
        maximum_output_tokens,
        state,
        cancellation_requested: row.get(13)?,
        failure,
        accepted_at_milliseconds: nonnegative_integer_from_row(row, 15)?,
        updated_at_milliseconds: nonnegative_integer_from_row(row, 16)?,
    })
}

pub(super) fn transcript_entry_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<TranscriptEntry> {
    let entry_sequence = nonnegative_integer_from_row(row, 0)?;
    let id = MessageId::from_bytes(row.get(1)?);
    let run_id = RunId::from_bytes(row.get(2)?);
    let entry_kind = row.get::<_, i64>(3)?;
    let service = row
        .get::<_, Option<i64>>(4)?
        .map(RunOpenCodeService::from_record)
        .transpose()?;
    let model_id = row.get::<_, Option<String>>(5)?;
    let text = row.get(6)?;
    let refusal = row.get(7)?;
    let created_at_milliseconds = nonnegative_integer_from_row(row, 8)?;
    match (entry_kind, service, model_id, refusal) {
        (1, None, None, false) => Ok(TranscriptEntry::UserMessage {
            entry_sequence,
            id,
            run_id,
            text,
            created_at_milliseconds,
        }),
        (2, Some(service), Some(model_id), refusal) => Ok(TranscriptEntry::AssistantMessage {
            entry_sequence,
            id,
            run_id,
            service,
            model_id,
            text,
            refusal,
            created_at_milliseconds,
        }),
        _ => Err(rusqlite::Error::InvalidColumnType(
            3,
            "entry_kind".to_owned(),
            rusqlite::types::Type::Integer,
        )),
    }
}

pub(super) fn positive_u16_from_row(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<u16> {
    let value = nonnegative_integer_from_row(row, index)?;
    u16::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            rusqlite::Error::IntegralValueOutOfRange(
                index,
                i64::try_from(value).unwrap_or(i64::MAX),
            )
        })
}

pub(super) fn positive_u32_from_row(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<u32> {
    let value = nonnegative_integer_from_row(row, index)?;
    u32::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            rusqlite::Error::IntegralValueOutOfRange(
                index,
                i64::try_from(value).unwrap_or(i64::MAX),
            )
        })
}
