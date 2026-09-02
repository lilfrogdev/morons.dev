use rusqlite::{Connection, OptionalExtension, params};

use super::{
    Backend,
    records::{load_session, sequence_to_sql},
};
use crate::persistence::{
    PersistenceError, SessionId, WorkspaceBlockReason, WorkspaceState, WorkspaceSummary,
};

const FACT_PREPARED: i64 = 1;
const FACT_COMPLETED: i64 = 3;
const FACT_NOT_APPLIED: i64 = 4;
const FACT_BLOCKED: i64 = 5;

impl Backend {
    pub(crate) fn workspace_summary(
        &self,
        session_id: SessionId,
    ) -> Result<WorkspaceSummary, PersistenceError> {
        if load_session(&self.connection, session_id)?.is_none() {
            return Err(PersistenceError::SessionNotFound);
        }
        workspace_summary_at_sequence(&self.connection, session_id, u64::MAX)
    }
}

pub(super) fn workspace_summary_at_sequence(
    connection: &Connection,
    session_id: SessionId,
    event_sequence: u64,
) -> Result<WorkspaceSummary, PersistenceError> {
    let event_sequence = if event_sequence == u64::MAX {
        i64::MAX
    } else {
        sequence_to_sql(event_sequence)?
    };
    let record = connection
        .query_row(
            "SELECT fact_kind, file_count, logical_bytes
             FROM repository_import_facts
             WHERE session_id = ?1 AND fact_sequence <= ?2 AND fact_kind IN (1, 3, 4, 5)
             ORDER BY fact_sequence DESC LIMIT 1",
            params![&session_id.as_bytes()[..], event_sequence],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            },
        )
        .optional()?;
    let summary = match record {
        None | Some((FACT_NOT_APPLIED, None, None)) => empty_workspace(),
        Some((FACT_PREPARED, None, None)) => WorkspaceSummary {
            state: WorkspaceState::Importing,
            file_count: 0,
            logical_bytes: 0,
            block_reason: None,
            blocked_run_id: None,
            blocked_tool: None,
        },
        Some((FACT_COMPLETED, Some(file_count), Some(logical_bytes))) => WorkspaceSummary {
            state: WorkspaceState::Ready,
            file_count: nonnegative_u64(file_count)?,
            logical_bytes: nonnegative_u64(logical_bytes)?,
            block_reason: None,
            blocked_run_id: None,
            blocked_tool: None,
        },
        Some((FACT_BLOCKED, None, None)) => WorkspaceSummary {
            state: WorkspaceState::Blocked,
            file_count: 0,
            logical_bytes: 0,
            block_reason: Some(WorkspaceBlockReason::InconsistentImportState),
            blocked_run_id: None,
            blocked_tool: None,
        },
        _ => {
            return Err(PersistenceError::InvalidState {
                reason: "repository import facts have an invalid workspace summary",
            });
        }
    };
    if summary.state != WorkspaceState::Ready {
        return Ok(summary);
    }
    let uncertainty = connection
        .query_row(
            "SELECT uncertain.run_id, call.tool_kind
             FROM tool_operation_facts AS uncertain
             JOIN tool_calls AS call ON call.call_id = uncertain.call_id
             WHERE uncertain.session_id = ?1
               AND uncertain.fact_kind = 6
               AND uncertain.fact_sequence <= ?2
               AND NOT EXISTS (
                   SELECT 1 FROM tool_uncertainty_acknowledgements AS acknowledgement
                   WHERE acknowledgement.run_id = uncertain.run_id
                     AND acknowledgement.fact_sequence <= ?2
               )
             ORDER BY uncertain.fact_sequence DESC LIMIT 1",
            params![&session_id.as_bytes()[..], event_sequence],
            |row| Ok((row.get::<_, [u8; 16]>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;
    let Some((run_id, tool_kind)) = uncertainty else {
        return Ok(summary);
    };
    let tool =
        crate::tools::ToolKind::from_record(tool_kind).ok_or(PersistenceError::InvalidState {
            reason: "an uncertain tool effect has an invalid tool kind",
        })?;
    Ok(WorkspaceSummary {
        state: WorkspaceState::Blocked,
        block_reason: Some(WorkspaceBlockReason::UncertainToolEffect),
        blocked_run_id: Some(crate::persistence::RunId::from_bytes(run_id)),
        blocked_tool: Some(tool),
        ..summary
    })
}

pub(super) fn workspace_summary_for_event(
    connection: &Connection,
    session_id: SessionId,
    event_id: &[u8; 16],
    event_sequence: u64,
) -> Result<WorkspaceSummary, PersistenceError> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM repository_import_facts
            WHERE session_id = ?1 AND delivery_event_id = ?2
              AND fact_sequence = ?3 AND fact_kind IN (1, 3, 4, 5)
            UNION ALL
            SELECT 1 FROM tool_operation_facts
            WHERE session_id = ?1 AND workspace_delivery_event_id = ?2
              AND fact_sequence = ?3 AND fact_kind = 6
            UNION ALL
            SELECT 1 FROM tool_uncertainty_acknowledgements
            WHERE session_id = ?1 AND delivery_event_id = ?2 AND fact_sequence = ?3
        )",
        params![
            &session_id.as_bytes()[..],
            &event_id[..],
            sequence_to_sql(event_sequence)?,
        ],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(PersistenceError::InvalidState {
            reason: "a workspace event is missing its canonical import fact",
        });
    }
    workspace_summary_at_sequence(connection, session_id, event_sequence)
}

fn empty_workspace() -> WorkspaceSummary {
    WorkspaceSummary {
        state: WorkspaceState::Empty,
        file_count: 0,
        logical_bytes: 0,
        block_reason: None,
        blocked_run_id: None,
        blocked_tool: None,
    }
}

fn nonnegative_u64(value: i64) -> Result<u64, PersistenceError> {
    u64::try_from(value).map_err(|_| PersistenceError::InvalidState {
        reason: "a historical repository import count is invalid",
    })
}
