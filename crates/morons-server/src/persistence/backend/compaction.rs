use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    Backend,
    records::{
        current_time_milliseconds, next_sequence, random_identifier, sequence_to_sql, time_to_sql,
    },
    run_records::load_required_run,
};
use crate::persistence::{
    CompactionOperationId, CompactionPlan, ContextCheckpoint, ContextCheckpointId,
    PersistenceError, PersistenceResourceLimit, RunId, RunOpenCodeService, SessionId,
};

const STATE_PREPARED: i64 = 1;
const STATE_DISPATCHED: i64 = 2;
const STATE_FAILED: i64 = 4;
const STATE_UNCERTAIN: i64 = 5;
const MAX_SUMMARY_BYTES: usize = super::context_budget::MAX_COMPACTION_SUMMARY_BYTES;

impl Backend {
    pub(super) fn validate_context_checkpoint_digests(&self) -> Result<(), PersistenceError> {
        let mut statement = self.connection.prepare(
            "SELECT checkpoint_id, session_id, source_entry_high_water, source_digest,
                    summary, estimated_summary_tokens
             FROM context_checkpoints ORDER BY session_id, source_entry_high_water",
        )?;
        let checkpoints = statement.query_map([], |row| {
            Ok((
                row.get::<_, [u8; 16]>(0)?,
                row.get::<_, [u8; 16]>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, [u8; 32]>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        for checkpoint in checkpoints {
            let (_, session_id, high_water, expected_digest, summary, summary_tokens) = checkpoint?;
            let session_id = SessionId::from_bytes(session_id);
            let high_water =
                u64::try_from(high_water).map_err(|_| PersistenceError::InvalidState {
                    reason: "a context checkpoint high water is invalid",
                })?;
            if self.context_digest_through(session_id, high_water)? != expected_digest
                || crate::persistence::run_types::conservative_input_token_estimate(
                    summary.len() as u64,
                    1,
                ) != u32::try_from(summary_tokens).ok()
            {
                return Err(PersistenceError::InvalidState {
                    reason: "a context checkpoint has an invalid source digest or token estimate",
                });
            }
        }
        Ok(())
    }

    pub(crate) fn prepare_auto_compaction(
        &mut self,
        run_id: RunId,
        plan: &CompactionPlan,
    ) -> Result<CompactionOperationId, PersistenceError> {
        if plan.source.is_empty() || plan.source_entry_high_water == 0 {
            return Err(PersistenceError::InvalidInput {
                reason: "a compaction plan has no source prefix",
            });
        }
        if let Some(existing) = self
            .connection
            .query_row(
                "SELECT operation_id, parent_checkpoint_id, source_entry_high_water, source_digest
                 FROM compaction_operations WHERE run_id = ?1",
                [&run_id.as_bytes()[..]],
                |row| {
                    Ok((
                        row.get::<_, [u8; 16]>(0)?,
                        row.get::<_, Option<[u8; 16]>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, [u8; 32]>(3)?,
                    ))
                },
            )
            .optional()?
        {
            if existing.1 != plan.parent_checkpoint_id.map(|id| *id.as_bytes())
                || u64::try_from(existing.2).ok() != Some(plan.source_entry_high_water)
                || existing.3 != plan.source_digest
            {
                return Err(PersistenceError::InvalidState {
                    reason: "a run compaction operation changed its source binding",
                });
            }
            return Ok(CompactionOperationId::from_bytes(existing.0));
        }
        let run = load_required_run(&self.connection, run_id)?;
        if plan.source_entry_high_water >= run.source_entry_high_water
            || self.context_digest_through(run.session_id, plan.source_entry_high_water)?
                != plan.source_digest
        {
            return Err(PersistenceError::InvalidState {
                reason: "a compaction source binding changed before preparation",
            });
        }
        if run.state != crate::persistence::RunState::Active {
            return Err(PersistenceError::InvalidState {
                reason: "only an active run can prepare automatic compaction",
            });
        }
        let operation_id = CompactionOperationId::from_bytes(random_identifier()?);
        let now = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sequence = next_sequence(&transaction)?;
        let parent_checkpoint_id = plan.parent_checkpoint_id.map(|id| *id.as_bytes());
        transaction.execute(
            "INSERT INTO compaction_operations (
                operation_id, run_id, session_id, parent_checkpoint_id,
                source_entry_high_water, source_digest, state, checkpoint_id,
                prepared_sequence, updated_sequence, prepared_at_milliseconds,
                updated_at_milliseconds
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, NULL, ?7, ?7, ?8, ?8)",
            params![
                &operation_id.as_bytes()[..],
                &run.id.as_bytes()[..],
                &run.session_id.as_bytes()[..],
                parent_checkpoint_id.as_ref().map(|id| &id[..]),
                sequence_to_sql(plan.source_entry_high_water)?,
                &plan.source_digest[..],
                sequence_to_sql(sequence)?,
                time_to_sql(now)?,
            ],
        )?;
        transaction.commit()?;
        Ok(operation_id)
    }

    pub(crate) fn mark_compaction_dispatched(
        &mut self,
        run_id: RunId,
        operation_id: CompactionOperationId,
    ) -> Result<(), PersistenceError> {
        self.transition_compaction(run_id, operation_id, STATE_PREPARED, STATE_DISPATCHED)
    }

    pub(crate) fn fail_compaction(
        &mut self,
        run_id: RunId,
        operation_id: CompactionOperationId,
        uncertain: bool,
    ) -> Result<(), PersistenceError> {
        self.transition_compaction(
            run_id,
            operation_id,
            if uncertain {
                STATE_DISPATCHED
            } else {
                STATE_PREPARED
            },
            if uncertain {
                STATE_UNCERTAIN
            } else {
                STATE_FAILED
            },
        )
    }

    fn transition_compaction(
        &mut self,
        run_id: RunId,
        operation_id: CompactionOperationId,
        expected: i64,
        target: i64,
    ) -> Result<(), PersistenceError> {
        let now = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sequence = next_sequence(&transaction)?;
        let changed = transaction.execute(
            "UPDATE compaction_operations
             SET state = ?1, updated_sequence = ?2, updated_at_milliseconds = ?3
             WHERE operation_id = ?4 AND run_id = ?5 AND state = ?6",
            params![
                target,
                sequence_to_sql(sequence)?,
                time_to_sql(now)?,
                &operation_id.as_bytes()[..],
                &run_id.as_bytes()[..],
                expected,
            ],
        )?;
        if changed != 1 {
            return Err(PersistenceError::InvalidState {
                reason: "a compaction operation transition is invalid",
            });
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn complete_compaction(
        &mut self,
        run_id: RunId,
        operation_id: CompactionOperationId,
        service: RunOpenCodeService,
        model_id: &str,
        summary: String,
    ) -> Result<ContextCheckpoint, PersistenceError> {
        if summary.is_empty() || summary.len() > MAX_SUMMARY_BYTES {
            return Err(PersistenceError::ResourceLimit {
                resource: PersistenceResourceLimit::Context,
            });
        }
        let estimated_summary_tokens =
            crate::persistence::run_types::conservative_input_token_estimate(
                summary.len() as u64,
                1,
            )
            .ok_or(PersistenceError::ResourceLimit {
                resource: PersistenceResourceLimit::Context,
            })?;
        let checkpoint_id = ContextCheckpointId::from_bytes(random_identifier()?);
        let now = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let operation = transaction
            .query_row(
                "SELECT session_id, parent_checkpoint_id, source_entry_high_water, source_digest
                 FROM compaction_operations
                 WHERE operation_id = ?1 AND run_id = ?2 AND state = 2",
                params![&operation_id.as_bytes()[..], &run_id.as_bytes()[..]],
                |row| {
                    Ok((
                        row.get::<_, [u8; 16]>(0)?,
                        row.get::<_, Option<[u8; 16]>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, [u8; 32]>(3)?,
                    ))
                },
            )
            .optional()?
            .ok_or(PersistenceError::InvalidState {
                reason: "a dispatched compaction operation is missing",
            })?;
        let sequence = next_sequence(&transaction)?;
        transaction.execute(
            "INSERT INTO context_checkpoints (
                checkpoint_id, session_id, parent_checkpoint_id, source_entry_high_water,
                source_digest, context_policy_version, open_code_service, model_id,
                summary, estimated_summary_tokens, fact_sequence, created_at_milliseconds
             ) VALUES (?1, ?2, ?3, ?4, ?5, 4, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                &checkpoint_id.as_bytes()[..],
                &operation.0[..],
                operation.1.as_ref().map(|id| &id[..]),
                operation.2,
                &operation.3[..],
                service.to_record(),
                model_id,
                &summary,
                i64::from(estimated_summary_tokens),
                sequence_to_sql(sequence)?,
                time_to_sql(now)?,
            ],
        )?;
        let changed = transaction.execute(
            "UPDATE compaction_operations
             SET state = 3, checkpoint_id = ?1, updated_sequence = ?2,
                 updated_at_milliseconds = ?3
             WHERE operation_id = ?4 AND run_id = ?5 AND state = 2",
            params![
                &checkpoint_id.as_bytes()[..],
                sequence_to_sql(sequence)?,
                time_to_sql(now)?,
                &operation_id.as_bytes()[..],
                &run_id.as_bytes()[..],
            ],
        )?;
        if changed != 1 {
            return Err(PersistenceError::InvalidState {
                reason: "a completed compaction operation lost its dispatch state",
            });
        }
        transaction.commit()?;
        Ok(ContextCheckpoint {
            id: checkpoint_id,
            source_entry_high_water: u64::try_from(operation.2).map_err(|_| {
                PersistenceError::InvalidState {
                    reason: "a compaction source high water is invalid",
                }
            })?,
            summary,
            estimated_summary_tokens,
        })
    }

    pub(super) fn recover_compaction_operations(&mut self) -> Result<(), PersistenceError> {
        let now = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pending: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM compaction_operations WHERE state IN (1, 2)",
            [],
            |row| row.get(0),
        )?;
        for _ in 0..pending {
            let sequence = next_sequence(&transaction)?;
            transaction.execute(
                "UPDATE compaction_operations
                 SET state = CASE state WHEN 1 THEN 4 ELSE 5 END,
                     updated_sequence = ?1, updated_at_milliseconds = ?2
                 WHERE operation_id = (
                     SELECT operation_id FROM compaction_operations
                     WHERE state IN (1, 2) ORDER BY prepared_sequence LIMIT 1
                 )",
                params![sequence_to_sql(sequence)?, time_to_sql(now)?],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
}
