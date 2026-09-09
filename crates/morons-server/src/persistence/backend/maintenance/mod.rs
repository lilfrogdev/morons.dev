mod execution;
mod installation;
mod observation;
mod records;

use rusqlite::{Connection, OptionalExtension as _, Transaction, TransactionBehavior, params};

use super::{
    Backend,
    records::{
        current_time_milliseconds, next_sequence, nonnegative_integer_from_row, sequence_to_sql,
        time_to_sql,
    },
};
use crate::persistence::{PersistenceError, Run, RunService, SessionId};
use records::{Job, MAX_JOBS, State, hash_parts, latest_parent, result_digest, validate_result};

impl Backend {
    pub(super) fn validate_maintenance_records(&self) -> Result<(), PersistenceError> {
        let count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM compaction_maintenance_jobs",
            [],
            |row| row.get(0),
        )?;
        let orphaned: bool = self.connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM compaction_maintenance_events AS event LEFT JOIN compaction_maintenance_jobs AS job
             ON job.job_id = event.job_id WHERE job.job_id IS NULL
             UNION ALL SELECT 1 FROM compaction_maintenance_jobs WHERE state IN (1, 2, 3)
                 GROUP BY session_id HAVING COUNT(*) > 1
             UNION ALL SELECT COUNT(*) FROM compaction_maintenance_jobs WHERE state = 2 HAVING COUNT(*) > 1)", [], |row| row.get(0),
        )?;
        if count > MAX_JOBS || orphaned {
            return Err(invalid());
        }
        if count > 0 {
            records::validate_sequences(&self.connection)?;
        }
        let mut after = 0;
        let mut visited = 0;
        loop {
            let jobs = job_page(&self.connection, after)?;
            if jobs.is_empty() {
                break;
            }
            for (id, sequence) in jobs {
                visited += 1;
                if visited > MAX_JOBS {
                    return Err(invalid());
                }
                let job = Job::load(&self.connection, id)?;
                self.validate_maintenance_binding(&job)?;
                validate_events(&self.connection, &job)?;
                after = sequence;
            }
        }
        if visited != count {
            return Err(invalid());
        }
        Ok(())
    }

    fn validate_maintenance_binding(&self, job: &Job) -> Result<(), PersistenceError> {
        if job.policy != 1
            || job.sequence == 0
            || job.source == 0
            || job.run.session_id != job.session
            || !job.run.state.is_terminal()
            || job.run.context_policy_version != 4
            || !matches!(
                (job.run.tool_catalog_version, job.run.tool_limits_version),
                (10, 10) | (11, 11)
            )
            || job.run.execution_image_generation.is_some()
            || job.source >= job.run.source_entry_high_water
            || job.through < job.run.source_entry_high_water
            || job
                .parent
                .as_ref()
                .is_some_and(|parent| parent.source_entry_high_water >= job.source)
            || job.digest() != job.binding_digest
        {
            return Err(invalid());
        }
        let policy_sequence: u64 = self.connection.query_row("SELECT COALESCE(MAX(accepted_sequence), 0) FROM data_use_policies WHERE accepted_sequence < ?1", [sequence_to_sql(job.sequence)?], |row| nonnegative_integer_from_row(row, 0))?;
        if policy_sequence != job.data_use_sequence {
            return Err(invalid());
        }
        let valid: bool = self.connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM session_run_states WHERE session_id = ?1 AND entry_high_water >= ?2)
             AND EXISTS (SELECT 1 FROM session_entries WHERE session_id = ?1 AND entry_sequence = ?3 + 1 AND entry_kind = 1)
             AND NOT EXISTS (SELECT 1 FROM session_entries WHERE run_id = ?4 AND entry_sequence > ?2)
             AND NOT EXISTS (SELECT 1 FROM session_entries WHERE session_id = ?1 AND entry_kind = 1
                 AND entry_sequence > ?5 AND entry_sequence <= ?2)
             AND NOT EXISTS (SELECT 1 FROM run_state_facts WHERE run_id = ?4 AND fact_sequence >= ?6)
             AND NOT EXISTS (SELECT 1 FROM session_entries WHERE session_id = ?1 AND (
                 (entry_sequence <= ?2 AND fact_sequence >= ?6) OR (entry_sequence > ?2 AND fact_sequence < ?6)))
             AND NOT EXISTS (SELECT 1 FROM local_commands WHERE session_id = ?1 AND (
                 (accepted_sequence < ?6 AND (state IN (1, 2) OR updated_sequence >= ?6 OR entry_sequence > ?2))
                 OR (entry_sequence <= ?2 AND updated_sequence >= ?6)))
             AND (SELECT checkpoint_id FROM context_checkpoints WHERE session_id = ?1 AND fact_sequence < ?6
                 ORDER BY source_entry_high_water DESC LIMIT 1) IS ?7
             AND NOT EXISTS (SELECT 1 FROM session_entries AS call WHERE call.session_id = ?1
                 AND call.entry_kind = 3 AND call.entry_sequence <= ?3 AND NOT EXISTS (
                     SELECT 1 FROM session_entries AS result WHERE result.tool_call_id = call.tool_call_id
                         AND result.entry_kind = 4 AND result.session_id = ?1 AND result.entry_sequence <= ?3))",
            params![&job.session.as_bytes()[..], sequence_to_sql(job.through)?, sequence_to_sql(job.source)?,
                &job.run.id.as_bytes()[..], sequence_to_sql(job.run.source_entry_high_water)?, sequence_to_sql(job.sequence)?,
                job.parent.as_ref().map(|parent| &parent.id.as_bytes()[..])],
            |row| row.get(0),
        )?;
        if !valid || self.context_digest_through(job.session, job.source)? != job.source_digest {
            return Err(invalid());
        }
        Ok(())
    }

    pub(super) fn maintenance_profile(
        &self,
        run: &Run,
    ) -> Result<Option<[u8; 32]>, PersistenceError> {
        let Some(model) =
            crate::provider::find_model_profile(run.service.model_service(), &run.model_id)
        else {
            return Ok(None);
        };
        if run.context_policy_version != crate::persistence::CONTEXT_POLICY_VERSION
            || run.tool_catalog_version != crate::tools::TOOL_CATALOG_VERSION
            || run.tool_limits_version != crate::tools::TOOL_LIMITS_VERSION
            || run.protocol_revision != model.protocol_revision
            || run.maximum_input_tokens != model.maximum_input_tokens
            || run.maximum_output_tokens != model.maximum_output_tokens
        {
            return Ok(None);
        }
        let skills = super::run_queries::load_run_skills(&self.connection, run.id)?
            .developer_text()
            .unwrap_or_default();
        let project = super::project_context::load(&self.connection, run.id)?
            .and_then(|context| context.developer_text())
            .unwrap_or_default();
        let tools = serde_json::Value::Array(crate::tools::provider_tools().map_err(|_| invalid())?
            .definitions().iter().map(|tool| serde_json::json!({
                "name": tool.name, "description": tool.description, "parameters": tool.parameters, "strict": tool.strict,
            })).collect()).to_string();
        Ok(Some(hash_parts(
            b"morons.dev/maintenance-instructions/v1\0",
            &[
                crate::prompts::COMPACTION.as_bytes(),
                crate::prompts::instruction(false).as_bytes(),
                skills.as_bytes(),
                project.as_bytes(),
                tools.as_bytes(),
                format!("{:?}", (model.protocol, model.capabilities, model.data_use)).as_bytes(),
            ],
        )))
    }

    pub(super) fn recover_maintenance_jobs(&mut self) -> Result<(), PersistenceError> {
        let mut after = 0;
        loop {
            let jobs = job_page(&self.connection, after)?;
            if jobs.is_empty() {
                break;
            }
            for (id, sequence) in jobs {
                let job = Job::load(&self.connection, id)?;
                let target = match job.state {
                    State::Prepared => Some(State::Cancelled),
                    State::Dispatched => Some(State::Uncertain),
                    State::Ready => {
                        let archived: bool = self.connection.query_row(
                            "SELECT archived FROM sessions WHERE session_id = ?1",
                            [&job.session.as_bytes()[..]],
                            |row| row.get(0),
                        )?;
                        let credential = self.model_credential_status(job.run.service)?;
                        (archived
                            || self.data_use_policy()?.sequence != job.data_use_sequence
                            || !credential.configured
                            || credential.generation != job.run.credential_generation
                            || latest_parent(&self.connection, job.session)?
                                != job.parent.as_ref().map(|parent| *parent.id.as_bytes())
                            || self.maintenance_profile(&job.run)? != Some(job.instruction_digest))
                        .then_some(State::Discarded)
                    }
                    _ => None,
                };
                if let Some(target) = target {
                    let transaction = self
                        .connection
                        .transaction_with_behavior(TransactionBehavior::Immediate)?;
                    transition(&transaction, &job, target, None)?;
                    transaction.commit()?;
                }
                after = sequence;
            }
        }
        Ok(())
    }

    pub(super) fn compaction_prefix_was_attempted(
        &self,
        session: SessionId,
        source: u64,
    ) -> Result<bool, PersistenceError> {
        self.connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM compaction_maintenance_jobs WHERE session_id = ?1 AND source_entry_high_water = ?2
             UNION ALL SELECT 1 FROM compaction_operations WHERE session_id = ?1 AND source_entry_high_water = ?2)",
            params![&session.as_bytes()[..], sequence_to_sql(source)?], |row| row.get(0),
        ).map_err(PersistenceError::from)
    }
}

fn job_page(connection: &Connection, after: u64) -> Result<Vec<([u8; 16], u64)>, PersistenceError> {
    let mut statement = connection.prepare("SELECT job_id, prepared_sequence FROM compaction_maintenance_jobs WHERE prepared_sequence > ?1 ORDER BY prepared_sequence LIMIT 32")?;
    statement
        .query_map([sequence_to_sql(after)?], |row| {
            Ok((row.get(0)?, nonnegative_integer_from_row(row, 1)?))
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(PersistenceError::from)
}

fn validate_events(connection: &Connection, job: &Job) -> Result<(), PersistenceError> {
    let mut state = State::Prepared;
    let mut sequence = job.sequence;
    let mut time = job.time;
    let mut statement = connection.prepare(
        "SELECT state, fact_sequence, created_at_milliseconds, result_payload, result_digest, length(CAST(result_payload AS BLOB)), checkpoint_id, installed_run_id, installed_entry_high_water
         FROM compaction_maintenance_events WHERE job_id = ?1 ORDER BY fact_sequence LIMIT 7",
    )?;
    let mut rows = statement.query([&job.id[..]])?;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        count += 1;
        let next = State::from_record(row.get(0)?)?;
        let next_sequence = nonnegative_integer_from_row(row, 1)?;
        let next_time = nonnegative_integer_from_row(row, 2)?;
        if row
            .get::<_, Option<i64>>(5)?
            .is_some_and(|bytes| !(2..=records::MAX_RESULT_BYTES as i64).contains(&bytes))
        {
            return Err(invalid());
        }
        let payload: Option<String> = row.get(3)?;
        let expected: Option<[u8; 32]> = row.get(4)?;
        if count > 6 || !state.permits(next) || next_sequence <= sequence || next_time < time {
            return Err(invalid());
        }
        if next == State::Installed {
            installation::validate_installation(connection, job, next_sequence)?;
        } else if row.get::<_, Option<[u8; 16]>>(6)?.is_some()
            || row.get::<_, Option<[u8; 16]>>(7)?.is_some()
            || row.get::<_, Option<i64>>(8)?.is_some()
        {
            return Err(invalid());
        }
        if next == State::Ready {
            let payload = payload.ok_or_else(invalid)?;
            validate_result(&payload, &job.run)?;
            if expected != Some(result_digest(&job.id, &payload)) {
                return Err(invalid());
            }
        } else if payload.is_some() || expected.is_some() {
            return Err(invalid());
        }
        state = next;
        sequence = next_sequence;
        time = next_time;
    }
    if state != job.state {
        return Err(invalid());
    }
    Ok(())
}

fn transition(
    transaction: &Transaction<'_>,
    job: &Job,
    target: State,
    result: Option<&str>,
) -> Result<(), PersistenceError> {
    if target == State::Installed
        || !job.state.permits(target)
        || (target == State::Ready) != result.is_some()
        || job.digest() != job.binding_digest
    {
        return Err(invalid());
    }
    if let Some(result) = result {
        validate_result(result, &job.run)?;
    }
    let previous_time: u64 = transaction.query_row(
        "SELECT COALESCE(MAX(created_at_milliseconds), ?2) FROM compaction_maintenance_events WHERE job_id = ?1",
        params![&job.id[..], time_to_sql(job.time)?], |row| nonnegative_integer_from_row(row, 0),
    )?;
    let now = current_time_milliseconds()?.max(previous_time);
    let sequence = next_sequence(transaction)?;
    let changed = transaction.execute(
        "UPDATE compaction_maintenance_jobs SET state = ?1 WHERE job_id = ?2 AND state = ?3",
        params![target as i64, &job.id[..], job.state as i64],
    )?;
    if changed != 1 {
        return Err(invalid());
    }
    transaction.execute(
        "INSERT INTO compaction_maintenance_events (job_id, state, fact_sequence, created_at_milliseconds, result_payload, result_digest)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![&job.id[..], target as i64, sequence_to_sql(sequence)?, time_to_sql(now)?, result, result.map(|payload| result_digest(&job.id, payload))],
    )?;
    Ok(())
}

pub(super) fn drain_for_archive(
    transaction: &Transaction<'_>,
    session: SessionId,
) -> Result<(), PersistenceError> {
    ensure_drained(transaction, session)?;
    let id = transaction.query_row("SELECT job_id FROM compaction_maintenance_jobs WHERE session_id = ?1 AND state IN (1, 3)", [&session.as_bytes()[..]], |row| row.get::<_, [u8; 16]>(0)).optional()?;
    if let Some(id) = id {
        let job = Job::load(transaction, id)?;
        let target = if job.state == State::Prepared {
            State::Cancelled
        } else {
            State::Discarded
        };
        transition(transaction, &job, target, None)?;
    }
    Ok(())
}

pub(super) fn ensure_drained(
    connection: &Connection,
    session: SessionId,
) -> Result<(), PersistenceError> {
    let dispatched: bool = connection.query_row("SELECT EXISTS (SELECT 1 FROM compaction_maintenance_jobs WHERE session_id = ?1 AND state = 2)", [&session.as_bytes()[..]], |row| row.get(0))?;
    if dispatched {
        return Err(PersistenceError::InvalidState {
            reason: "session maintenance execution has not drained",
        });
    }
    Ok(())
}

fn unchanged(connection: &Connection, expected: Option<i64>) -> Result<(), PersistenceError> {
    let version: i64 = connection.query_row("PRAGMA data_version", [], |row| row.get(0))?;
    if Some(version) != expected {
        return Err(invalid());
    }
    Ok(())
}

fn invalid() -> PersistenceError {
    PersistenceError::InvalidState {
        reason: "compaction maintenance evidence has invalid bounds, scope, state or integrity",
    }
}

#[cfg(test)]
mod tests;
