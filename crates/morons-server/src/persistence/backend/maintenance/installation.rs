use super::super::{run_queries::load_run_skills, run_records::load_required_run};
use super::*;
use crate::persistence::{RunId, RunState, maintenance::MaintenanceResult};

impl Backend {
    pub(crate) fn maintenance_boundary(
        &mut self,
        run_id: RunId,
    ) -> Result<Option<SessionId>, PersistenceError> {
        self.ensure_context_integrity()?;
        let run = load_required_run(&self.connection, run_id)?;
        let prompt: String = self.connection.query_row(
            "SELECT text FROM session_entries WHERE run_id = ?1 AND entry_kind = 1",
            [&run.id.as_bytes()[..]],
            |row| row.get(0),
        )?;
        if prompt == "/compact" || prompt.starts_with("/compact ") {
            return Ok(Some(run.session_id));
        }
        if !self.maintenance_enabled
            || run.state != RunState::Active
            || run.cancellation_requested
            || run.provider_turns != 0
        {
            return Ok(None);
        }
        let id = self.connection.query_row("SELECT job_id FROM compaction_maintenance_jobs WHERE session_id = ?1 AND state = 3", [&run.session_id.as_bytes()[..]], |row| row.get::<_, [u8; 16]>(0)).optional()?;
        let Some(id) = id else {
            return Ok(None);
        };
        let job = Job::load(&self.connection, id)?;
        self.validate_maintenance_binding(&job)?;
        validate_events(&self.connection, &job)?;
        let result = ready_result(&self.connection, &job)?;
        let credential = self.open_code_credential_status()?;
        let through = self.connection.query_row(
            "SELECT entry_high_water FROM session_run_states WHERE session_id = ?1",
            [&run.session_id.as_bytes()[..]],
            |row| nonnegative_integer_from_row(row, 0),
        )?;
        let pending: bool = self.connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM provider_operation_facts WHERE run_id = ?1 AND fact_kind = 1)
             OR EXISTS (SELECT 1 FROM tool_calls WHERE run_id = ?1)", [&run.id.as_bytes()[..]], |row| row.get(0),
        )?;
        if pending || through != run.source_entry_high_water {
            return Ok(None);
        }
        let skills = load_run_skills(&self.connection, run.id)?;
        let project = super::super::project_context::load(&self.connection, run.id)?;
        let instructions = skills.context_bytes().ok_or_else(invalid)?.saturating_add(
            project
                .as_ref()
                .map_or(0, |project| project.context_bytes()),
        );
        let before = self.context_budget(
            run.session_id,
            job.parent
                .as_ref()
                .map_or(0, |parent| parent.source_entry_high_water),
            through,
        )?;
        let after = self.context_budget(run.session_id, job.source, through)?;
        let compatible = credential.configured
            && credential.generation == run.credential_generation
            && run.credential_generation == job.run.credential_generation
            && run.service == job.run.service
            && run.model_id == job.run.model_id
            && run.protocol_revision == job.run.protocol_revision
            && run.maximum_input_tokens == job.run.maximum_input_tokens
            && run.maximum_output_tokens == job.run.maximum_output_tokens
            && job.source < run.source_entry_high_water
            && latest_parent(&self.connection, job.session)?
                == job.parent.as_ref().map(|parent| *parent.id.as_bytes())
            && self.maintenance_profile(&run)? == Some(job.instruction_digest)
            && after.fits(
                run.maximum_input_tokens,
                instructions + result.summary.len(),
            )
            && execution::useful(
                &before,
                &after,
                instructions + job.parent.as_ref().map_or(0, |parent| parent.summary.len()),
                instructions + result.summary.len(),
            );
        let version = self.context_data_version.get();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        unchanged(&transaction, version)?;
        if !compatible {
            transition(&transaction, &job, State::Discarded, None)?;
            transaction.commit()?;
            return Ok(None);
        }
        let checkpoint = super::super::records::random_identifier()?;
        let checkpoint_sequence = next_sequence(&transaction)?;
        let now = current_time_milliseconds()?.max(job.time);
        let tokens =
            crate::persistence::conservative_input_token_estimate(result.summary.len() as u64, 1)
                .ok_or_else(invalid)?;
        transaction.execute(
            "INSERT INTO context_checkpoints (checkpoint_id, session_id, parent_checkpoint_id, source_entry_high_water, source_digest,
                context_policy_version, open_code_service, model_id, summary, estimated_summary_tokens, fact_sequence, created_at_milliseconds)
             VALUES (?1, ?2, ?3, ?4, ?5, 4, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![&checkpoint[..], &job.session.as_bytes()[..], job.parent.as_ref().map(|parent| &parent.id.as_bytes()[..]), sequence_to_sql(job.source)?,
                &job.source_digest[..], job.run.service.to_record(), &job.run.model_id, &result.summary, tokens, sequence_to_sql(checkpoint_sequence)?, time_to_sql(now)?],
        )?;
        let sequence = next_sequence(&transaction)?;
        let event_time: u64 = transaction.query_row("SELECT MAX(created_at_milliseconds) FROM compaction_maintenance_events WHERE job_id = ?1", [&id[..]], |row| nonnegative_integer_from_row(row, 0))?;
        transaction.execute("INSERT INTO compaction_maintenance_events (job_id, state, fact_sequence, created_at_milliseconds, checkpoint_id, installed_run_id, installed_entry_high_water)
            VALUES (?1, 8, ?2, ?3, ?4, ?5, ?6)", params![&id[..], sequence_to_sql(sequence)?, time_to_sql(now.max(event_time))?, &checkpoint[..], &run.id.as_bytes()[..], sequence_to_sql(through)?])?;
        if transaction.execute(
            "UPDATE compaction_maintenance_jobs SET state = 8 WHERE job_id = ?1 AND state = 3",
            [&id[..]],
        )? != 1
        {
            return Err(invalid());
        }
        transaction.commit()?;
        Ok(None)
    }
}

fn ready_result(connection: &Connection, job: &Job) -> Result<MaintenanceResult, PersistenceError> {
    let payload: String = connection.query_row(
        "SELECT result_payload FROM compaction_maintenance_events WHERE job_id = ?1 AND state = 3",
        [&job.id[..]],
        |row| row.get(0),
    )?;
    validate_result(&payload, &job.run)?;
    serde_json::from_str(&payload).map_err(|_| invalid())
}

pub(super) fn validate_installation(
    connection: &Connection,
    job: &Job,
    sequence: u64,
) -> Result<(), PersistenceError> {
    let result = ready_result(connection, job)?;
    let valid: bool = connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM compaction_maintenance_events AS event
         JOIN context_checkpoints AS checkpoint ON checkpoint.checkpoint_id = event.checkpoint_id
         JOIN run_accepted_facts AS run ON run.run_id = event.installed_run_id
         WHERE event.job_id = ?1 AND event.state = 8 AND event.fact_sequence = ?2
             AND checkpoint.session_id = ?3 AND checkpoint.parent_checkpoint_id IS ?4
             AND checkpoint.source_entry_high_water = ?5 AND checkpoint.source_digest = ?6
             AND checkpoint.open_code_service = ?7 AND checkpoint.model_id = ?8 AND checkpoint.context_policy_version = 4
             AND checkpoint.summary = ?9 AND checkpoint.fact_sequence > (SELECT fact_sequence FROM compaction_maintenance_events WHERE job_id = ?1 AND state = 3)
             AND checkpoint.fact_sequence < event.fact_sequence AND run.session_id = ?3 AND run.source_entry_high_water > ?5
             AND run.open_code_service = ?7 AND run.model_id = ?8 AND run.credential_generation = ?10
             AND run.protocol_revision = ?11 AND run.maximum_input_tokens = ?12 AND run.maximum_output_tokens = ?13
             AND event.installed_entry_high_water = run.source_entry_high_water
             AND run.context_policy_version = ?14 AND run.tool_catalog_version = ?15 AND run.tool_limits_version = ?16
             AND (SELECT state FROM run_state_facts WHERE run_id = run.run_id AND fact_sequence < checkpoint.fact_sequence ORDER BY fact_sequence DESC LIMIT 1) = 2
             AND NOT EXISTS (SELECT 1 FROM run_cancellation_requests WHERE run_id = run.run_id AND fact_sequence < event.fact_sequence)
             AND NOT EXISTS (SELECT 1 FROM provider_operation_facts WHERE run_id = run.run_id AND fact_kind = 1 AND fact_sequence <= event.fact_sequence))",
        params![&job.id[..], sequence_to_sql(sequence)?, &job.session.as_bytes()[..], job.parent.as_ref().map(|parent| &parent.id.as_bytes()[..]),
            sequence_to_sql(job.source)?, &job.source_digest[..], job.run.service.to_record(), &job.run.model_id, &result.summary,
            sequence_to_sql(job.run.credential_generation)?, job.run.protocol_revision, job.run.maximum_input_tokens, job.run.maximum_output_tokens,
            job.run.context_policy_version, job.run.tool_catalog_version, job.run.tool_limits_version],
        |row| row.get(0),
    )?;
    if !valid {
        return Err(invalid());
    }
    let receiving = connection.query_row("SELECT installed_run_id FROM compaction_maintenance_events WHERE job_id = ?1 AND state = 8", [&job.id[..]], |row| row.get::<_, [u8; 16]>(0))?;
    let receiving = RunId::from_bytes(receiving);
    if load_run_skills(connection, receiving)? != load_run_skills(connection, job.run.id)?
        || super::super::project_context::load(connection, receiving)?
            != super::super::project_context::load(connection, job.run.id)?
    {
        return Err(invalid());
    }
    Ok(())
}
