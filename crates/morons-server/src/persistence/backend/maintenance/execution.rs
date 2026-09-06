use super::super::{
    context_budget::{ContextBudget, MAX_COMPACTION_SUMMARY_BYTES},
    run_queries::{load_latest_checkpoint, load_run_skills},
    run_records::load_required_run,
};
use super::*;
use crate::persistence::{
    RunId, RunState,
    maintenance::{MaintenanceResult, MaintenanceWork},
};

impl Backend {
    pub(crate) fn configure_maintenance(&mut self, enabled: bool) -> Result<(), PersistenceError> {
        self.maintenance_enabled = enabled;
        if !enabled {
            let ids = {
                let mut statement = self.connection.prepare(
                    "SELECT job_id FROM compaction_maintenance_jobs WHERE state IN (1, 3)",
                )?;
                statement
                    .query_map([], |row| row.get::<_, [u8; 16]>(0))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            for id in ids {
                let job = Job::load(&self.connection, id)?;
                let target = if job.state == State::Prepared {
                    State::Cancelled
                } else {
                    State::Discarded
                };
                let version = self.context_data_version.get();
                let transaction = self
                    .connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                unchanged(&transaction, version)?;
                transition(&transaction, &job, target, None)?;
                transaction.commit()?;
            }
        }
        Ok(())
    }

    pub(crate) fn prepare_maintenance(
        &mut self,
        run_id: RunId,
    ) -> Result<Option<MaintenanceWork>, PersistenceError> {
        if !self.maintenance_enabled {
            return Ok(None);
        }
        self.ensure_context_integrity()?;
        let run = load_required_run(&self.connection, run_id)?;
        if run.state != RunState::Succeeded || !self.maintenance_idle(&run, None)? {
            return Ok(None);
        }
        let credential = self.open_code_credential_status()?;
        if !credential.configured || credential.generation != run.credential_generation {
            return Ok(None);
        }
        let Some(profile) = self.maintenance_profile(&run)? else {
            return Ok(None);
        };
        let (count, busy): (i64, bool) = self.connection.query_row(
            "SELECT (SELECT COUNT(*) FROM compaction_maintenance_jobs), EXISTS (SELECT 1 FROM compaction_maintenance_jobs
             WHERE (session_id = ?1 AND state IN (1, 2, 3)) OR state = 2)", [&run.session_id.as_bytes()[..]], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if count >= MAX_JOBS || busy {
            return Ok(None);
        }
        let through = self.connection.query_row(
            "SELECT entry_high_water FROM session_run_states WHERE session_id = ?1",
            [&run.session_id.as_bytes()[..]],
            |row| nonnegative_integer_from_row(row, 0),
        )?;
        let parent = load_latest_checkpoint(&self.connection, run.session_id, through)?;
        let covered = parent
            .as_ref()
            .map_or(0, |parent| parent.source_entry_high_water);
        let skills = load_run_skills(&self.connection, run.id)?;
        let project = super::super::project_context::load(&self.connection, run.id)?;
        let instructions = skills.context_bytes().ok_or_else(invalid)?.saturating_add(
            project
                .as_ref()
                .map_or(0, |project| project.context_bytes()),
        );
        let mut before = self.context_budget(run.session_id, covered, through)?;
        before.observed_input_tokens = self
            .observe_context_usage(
                run.session_id,
                super::super::context_usage::ContextModel {
                    service: run.service,
                    model_id: &run.model_id,
                    protocol_revision: run.protocol_revision,
                },
                parent.as_ref(),
                through,
                &skills,
                project.as_ref(),
            )?
            .map(|usage| usage.estimated_tokens);
        let old_extra = instructions + parent.as_ref().map_or(0, |parent| parent.summary.len());
        if !early_pressure(&before, run.maximum_input_tokens, old_extra) {
            return Ok(None);
        }
        let Some(source) = self.select_compaction_prefix_fitting(
            run.session_id,
            covered,
            run.source_entry_high_water,
            through,
            |tail| {
                ready_budget(
                    tail,
                    run.maximum_input_tokens,
                    instructions + MAX_COMPACTION_SUMMARY_BYTES,
                )
            },
        )?
        else {
            return Ok(None);
        };
        if self.compaction_prefix_was_attempted(run.session_id, source)? {
            return Ok(None);
        }
        let after = self.context_budget(run.session_id, source, through)?;
        if !useful(
            &before,
            &after,
            old_extra,
            instructions + MAX_COMPACTION_SUMMARY_BYTES,
        ) {
            return Ok(None);
        }
        let plan = self.project_compaction_prefix(run.session_id, parent.as_ref(), source, None)?;
        let id = super::super::records::random_identifier()?;
        let version = self.context_data_version.get();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        unchanged(&transaction, version)?;
        let mut job = Job {
            id,
            session: run.session_id,
            run: run.clone(),
            parent,
            source,
            through,
            source_digest: plan.source_digest,
            instruction_digest: profile,
            binding_digest: [0; 32],
            policy: 1,
            state: State::Prepared,
            sequence: next_sequence(&transaction)?,
            time: current_time_milliseconds()?,
        };
        job.binding_digest = job.digest();
        transaction.execute(
            "INSERT INTO compaction_maintenance_jobs (job_id, session_id, trigger_run_id, parent_checkpoint_id, source_entry_high_water,
                prepared_entry_high_water, source_digest, instruction_digest, binding_digest, maintenance_policy_version, state, prepared_sequence, prepared_at_milliseconds)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, 1, ?10, ?11)",
            params![&id[..], &run.session_id.as_bytes()[..], &run.id.as_bytes()[..], job.parent.as_ref().map(|parent| &parent.id.as_bytes()[..]),
                sequence_to_sql(source)?, sequence_to_sql(through)?, &job.source_digest[..], &profile[..], &job.binding_digest[..], sequence_to_sql(job.sequence)?, time_to_sql(job.time)?],
        )?;
        transaction.commit()?;
        Ok(Some(MaintenanceWork { id, run, plan }))
    }

    fn maintenance_idle(&self, run: &Run, through: Option<u64>) -> Result<bool, PersistenceError> {
        self.connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM sessions AS session JOIN session_run_states AS state USING (session_id)
             WHERE session.session_id = ?1 AND session.archived = 0 AND state.active_run_id IS NULL
                 AND (?3 IS NULL OR state.entry_high_water = ?3))
             AND (SELECT run_id FROM run_accepted_facts WHERE session_id = ?1 ORDER BY fact_sequence DESC LIMIT 1) = ?2
             AND NOT EXISTS (SELECT 1 FROM local_commands WHERE session_id = ?1 AND state IN (1, 2))
             AND NOT EXISTS (SELECT 1 FROM session_archive_requests WHERE session_id = ?1 AND archived = 1 AND state = 1)
             AND NOT EXISTS (SELECT 1 FROM session_delete_requests WHERE session_id = ?1 AND state < 3)",
            params![&run.session_id.as_bytes()[..], &run.id.as_bytes()[..], through.map(sequence_to_sql).transpose()?], |row| row.get(0),
        ).map_err(PersistenceError::from)
    }

    pub(crate) fn dispatch_maintenance(&mut self, id: [u8; 16]) -> Result<bool, PersistenceError> {
        self.ensure_context_integrity()?;
        let job = Job::load(&self.connection, id)?;
        if job.state != State::Prepared {
            return Ok(false);
        }
        self.validate_maintenance_binding(&job)?;
        let credential = self.open_code_credential_status()?;
        let allowed = self.maintenance_enabled
            && self.maintenance_idle(&job.run, Some(job.through))?
            && credential.configured
            && credential.generation == job.run.credential_generation
            && latest_parent(&self.connection, job.session)?
                == job.parent.as_ref().map(|parent| *parent.id.as_bytes())
            && self.maintenance_profile(&job.run)? == Some(job.instruction_digest);
        let version = self.context_data_version.get();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        unchanged(&transaction, version)?;
        transition(
            &transaction,
            &job,
            if allowed {
                State::Dispatched
            } else {
                State::Cancelled
            },
            None,
        )?;
        transaction.commit()?;
        Ok(allowed)
    }

    pub(crate) fn complete_maintenance(
        &mut self,
        id: [u8; 16],
        result: MaintenanceResult,
    ) -> Result<(), PersistenceError> {
        self.ensure_context_integrity()?;
        let job = Job::load(&self.connection, id)?;
        let payload = serde_json::to_string(&result).map_err(|_| invalid())?;
        let valid = validate_result(&payload, &job.run).is_ok();
        let version = self.context_data_version.get();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        unchanged(&transaction, version)?;
        transition(
            &transaction,
            &job,
            if valid {
                State::Ready
            } else {
                State::Uncertain
            },
            valid.then_some(payload.as_str()),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn finish_maintenance(
        &mut self,
        session: SessionId,
        failed: bool,
        discard_ready: bool,
    ) -> Result<(), PersistenceError> {
        self.ensure_context_integrity()?;
        let id = self.connection.query_row("SELECT job_id FROM compaction_maintenance_jobs WHERE session_id = ?1 AND state IN (1, 2, 3)", [&session.as_bytes()[..]], |row| row.get::<_, [u8; 16]>(0)).optional()?;
        if let Some(id) = id {
            let job = Job::load(&self.connection, id)?;
            let target = match job.state {
                State::Prepared if failed => State::Failed,
                State::Prepared => State::Cancelled,
                State::Dispatched => State::Uncertain,
                State::Ready if discard_ready => State::Discarded,
                _ => return Ok(()),
            };
            let version = self.context_data_version.get();
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            unchanged(&transaction, version)?;
            transition(&transaction, &job, target, None)?;
            transaction.commit()?;
        }
        Ok(())
    }
}

pub(super) fn useful(
    before: &ContextBudget,
    after: &ContextBudget,
    old_extra: usize,
    new_extra: usize,
) -> bool {
    before
        .tokens(old_extra)
        .saturating_sub(after.tokens(new_extra))
        >= 4096
        || before.entries.saturating_sub(after.entries) >= 24
        || before.images > after.images
        || before.image_bytes > after.image_bytes
}

fn thresholds(maximum: u32) -> (u64, u64) {
    let soft = u64::from(maximum) * 7 / 10;
    (soft, (soft / 8).clamp(8192, 32_000))
}

pub(super) fn ready_budget(budget: &ContextBudget, maximum: u32, extra: usize) -> bool {
    let (soft, lead) = thresholds(maximum);
    budget.tokens(extra) <= soft.saturating_sub(lead) && !budget.pressure(maximum, extra)
}

fn early_pressure(budget: &ContextBudget, maximum: u32, extra: usize) -> bool {
    let (soft, lead) = thresholds(maximum);
    budget.pressure(maximum, extra)
        || budget.estimated_tokens(extra) >= soft.saturating_sub(lead)
        || budget.tokens(extra) >= u64::from(maximum).saturating_sub(lead)
        || budget.entries + budget.images + super::super::context_budget::CONTEXT_ITEM_RESERVE
            >= 160
        || budget.images >= crate::persistence::images::MAX_CONTEXT_IMAGES as u64 * 5 / 8
        || budget.image_bytes >= crate::persistence::images::MAX_CONTEXT_IMAGE_BYTES * 5 / 8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_requires_headroom_below_independent_pressure() {
        assert!(ready_budget(&ContextBudget::default(), 96_000, 16_384));
        for budget in [
            ContextBudget {
                bytes: 60_000,
                ..Default::default()
            },
            ContextBudget {
                entries: 168,
                ..Default::default()
            },
            ContextBudget {
                image_bytes: crate::persistence::images::MAX_CONTEXT_IMAGE_BYTES * 3 / 4,
                ..Default::default()
            },
        ] {
            assert!(budget.fits(96_000, 0));
            assert!(!ready_budget(&budget, 96_000, 0));
        }
    }

    #[test]
    fn low_advisory_usage_cannot_hide_the_approaching_conservative_guard() {
        let budget = ContextBudget {
            bytes: 80_000,
            observed_input_tokens: Some(20_000),
            ..Default::default()
        };
        assert!(budget.fits(96_000, 0));
        assert!(!budget.pressure(96_000, 0));
        assert!(early_pressure(&budget, 96_000, 0));
        assert!(!early_pressure(
            &ContextBudget {
                bytes: 40_000,
                observed_input_tokens: Some(10_000),
                ..Default::default()
            },
            96_000,
            0
        ));
    }
}
