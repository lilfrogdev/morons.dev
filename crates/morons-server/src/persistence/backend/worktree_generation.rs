use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    Backend,
    records::{
        current_time_milliseconds, next_sequence, random_identifier, sequence_to_sql, time_to_sql,
    },
};
use crate::persistence::{
    PersistenceError, RepositoryImportOutcome, SessionId, WorktreeLayoutPlan,
};

const LAYOUT_PREPARED: u8 = 0;
const LAYOUT_DISPATCHED: u8 = 1;

impl Backend {
    pub(super) fn recover_worktree_layouts(&mut self) -> Result<(), PersistenceError> {
        let plans = self.worktree_layouts_for_recovery()?;
        let paths = self.paths.clone();
        for plan in plans {
            let plan = self.dispatch_worktree_layout(plan)?;
            match paths.migrate_worktree_layout(plan)? {
                crate::persistence::workspace::WorktreeLayoutRecovery::Complete(outcome) => {
                    self.complete_worktree_layout(plan, outcome)?;
                    paths.cleanup_legacy_worktree(&plan.workspace_id)?;
                }
                crate::persistence::workspace::WorktreeLayoutRecovery::Blocked => {
                    self.block_worktree_layout(plan)?;
                }
            }
        }
        for (plan, outcome) in self.ready_worktree_layouts()? {
            let path = self
                .paths
                .worktree_generation_path(&plan.workspace_id, &plan.generation_id);
            if !path.is_dir() {
                return Err(PersistenceError::InvalidState {
                    reason: "an active worktree generation is missing",
                });
            }
            let _ = outcome;
            paths.cleanup_legacy_worktree(&plan.workspace_id)?;
        }
        Ok(())
    }

    pub(crate) fn active_worktree_generation(
        &self,
        workspace_id: &[u8; 16],
    ) -> Result<[u8; 16], PersistenceError> {
        self.connection
            .query_row(
                "SELECT generation_id FROM active_worktree_generations WHERE workspace_id = ?1",
                [&workspace_id[..]],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(PersistenceError::WorkspaceBlocked)
    }

    fn dispatch_worktree_layout(
        &mut self,
        plan: WorktreeLayoutPlan,
    ) -> Result<WorktreeLayoutPlan, PersistenceError> {
        if plan.state == LAYOUT_DISPATCHED {
            return Ok(plan);
        }
        if plan.state != LAYOUT_PREPARED {
            return Err(PersistenceError::InvalidState {
                reason: "a worktree layout has an invalid dispatch state",
            });
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sequence = next_sequence(&transaction)?;
        let updated = transaction.execute(
            "UPDATE workspace_generation_layouts SET state = 1, updated_sequence = ?2
             WHERE workspace_id = ?1 AND state = 0",
            params![&plan.workspace_id[..], sequence_to_sql(sequence)?],
        )?;
        if updated != 1 {
            return Err(PersistenceError::InvalidState {
                reason: "a worktree layout did not reach dispatched state",
            });
        }
        transaction.commit()?;
        Ok(WorktreeLayoutPlan {
            state: LAYOUT_DISPATCHED,
            ..plan
        })
    }

    fn complete_worktree_layout(
        &mut self,
        plan: WorktreeLayoutPlan,
        outcome: RepositoryImportOutcome,
    ) -> Result<(), PersistenceError> {
        let fact_id = random_identifier()?;
        let created_at = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sequence = next_sequence(&transaction)?;
        let updated = transaction.execute(
            "UPDATE workspace_generation_layouts
             SET state = 2, updated_sequence = ?2, file_count = ?3,
                 directory_count = ?4, logical_bytes = ?5, manifest_digest = ?6
             WHERE workspace_id = ?1 AND generation_id = ?7 AND state = 1",
            params![
                &plan.workspace_id[..],
                sequence_to_sql(sequence)?,
                sequence_to_sql(outcome.file_count)?,
                sequence_to_sql(outcome.directory_count)?,
                sequence_to_sql(outcome.logical_bytes)?,
                &outcome.manifest_digest[..],
                &plan.generation_id[..],
            ],
        )?;
        if updated != 1 {
            return Err(PersistenceError::InvalidState {
                reason: "a worktree layout did not reach ready state",
            });
        }
        transaction.execute(
            "INSERT INTO worktree_generation_facts (
                fact_id, fact_sequence, session_id, workspace_id, generation_id,
                predecessor_generation_id, publication_kind, operation_id,
                file_count, directory_count, logical_bytes, manifest_digest,
                created_at_milliseconds
             ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, 1, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                &fact_id[..],
                sequence_to_sql(sequence)?,
                &plan.session_id.as_bytes()[..],
                &plan.workspace_id[..],
                &plan.generation_id[..],
                &plan.operation_id[..],
                sequence_to_sql(outcome.file_count)?,
                sequence_to_sql(outcome.directory_count)?,
                sequence_to_sql(outcome.logical_bytes)?,
                &outcome.manifest_digest[..],
                time_to_sql(created_at)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO active_worktree_generations (
                workspace_id, session_id, generation_id, updated_sequence
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                &plan.workspace_id[..],
                &plan.session_id.as_bytes()[..],
                &plan.generation_id[..],
                sequence_to_sql(sequence)?,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn block_worktree_layout(&mut self, plan: WorktreeLayoutPlan) -> Result<(), PersistenceError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sequence = next_sequence(&transaction)?;
        let updated = transaction.execute(
            "UPDATE workspace_generation_layouts SET state = 3, updated_sequence = ?2
             WHERE workspace_id = ?1 AND state IN (0, 1)",
            params![&plan.workspace_id[..], sequence_to_sql(sequence)?],
        )?;
        if updated != 1 {
            return Err(PersistenceError::InvalidState {
                reason: "a worktree layout did not reach blocked state",
            });
        }
        transaction.commit()?;
        Ok(())
    }

    fn worktree_layouts_for_recovery(&self) -> Result<Vec<WorktreeLayoutPlan>, PersistenceError> {
        let mut statement = self.connection.prepare(
            "SELECT session_id, workspace_id, generation_id, operation_id, state
             FROM workspace_generation_layouts WHERE state IN (0, 1)
             ORDER BY created_sequence",
        )?;
        statement
            .query_map([], layout_from_row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(PersistenceError::from)
    }

    fn ready_worktree_layouts(
        &self,
    ) -> Result<Vec<(WorktreeLayoutPlan, RepositoryImportOutcome)>, PersistenceError> {
        let mut statement = self.connection.prepare(
            "SELECT session_id, workspace_id, generation_id, operation_id, state,
                    file_count, directory_count, logical_bytes, manifest_digest
             FROM workspace_generation_layouts WHERE state = 2 ORDER BY created_sequence",
        )?;
        statement
            .query_map([], |row| {
                let plan = layout_from_row(row)?;
                Ok((
                    plan,
                    RepositoryImportOutcome {
                        file_count: nonnegative(row.get(5)?, 5)?,
                        directory_count: nonnegative(row.get(6)?, 6)?,
                        logical_bytes: nonnegative(row.get(7)?, 7)?,
                        manifest_digest: row.get(8)?,
                    },
                ))
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(PersistenceError::from)
    }
}

fn layout_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorktreeLayoutPlan> {
    let state = row.get::<_, i64>(4)?;
    Ok(WorktreeLayoutPlan {
        session_id: SessionId::from_bytes(row.get(0)?),
        workspace_id: row.get(1)?,
        generation_id: row.get(2)?,
        operation_id: row.get(3)?,
        state: u8::try_from(state)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(4, state))?,
    })
}

fn nonnegative(value: i64, column: usize) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(column, value))
}
