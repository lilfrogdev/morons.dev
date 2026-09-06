use crate::persistence::maintenance::MaintenanceResult;
use rusqlite::{Connection, OptionalExtension as _, params};
use sha2::{Digest as _, Sha256};

use super::{super::run_records::load_required_run, invalid, nonnegative_integer_from_row};
use crate::persistence::{
    ContextCheckpoint, ContextCheckpointId, PersistenceError, Run, RunId, SessionId,
};

pub(super) const MAX_JOBS: i64 = 10_000;
pub(super) const MAX_RESULT_BYTES: usize = 100 * 1024;

pub(super) use crate::persistence::maintenance::MaintenanceState as State;

impl State {
    pub(super) fn from_record(value: i64) -> Result<Self, PersistenceError> {
        match value {
            1 => Ok(Self::Prepared),
            2 => Ok(Self::Dispatched),
            3 => Ok(Self::Ready),
            4 => Ok(Self::Failed),
            5 => Ok(Self::Cancelled),
            6 => Ok(Self::Uncertain),
            7 => Ok(Self::Discarded),
            8 => Ok(Self::Installed),
            _ => Err(invalid()),
        }
    }

    pub(super) fn permits(self, next: Self) -> bool {
        matches!(
            (self, next),
            (
                Self::Prepared,
                Self::Dispatched | Self::Failed | Self::Cancelled
            ) | (Self::Dispatched, Self::Ready | Self::Uncertain)
                | (Self::Ready, Self::Discarded | Self::Installed)
        )
    }
}

pub(super) struct Job {
    pub id: [u8; 16],
    pub session: SessionId,
    pub run: Run,
    pub parent: Option<ContextCheckpoint>,
    pub source: u64,
    pub through: u64,
    pub source_digest: [u8; 32],
    pub instruction_digest: [u8; 32],
    pub binding_digest: [u8; 32],
    pub policy: u16,
    pub state: State,
    pub sequence: u64,
    pub time: u64,
}

impl Job {
    pub(super) fn load(connection: &Connection, id: [u8; 16]) -> Result<Self, PersistenceError> {
        let row = connection.query_row(
            "SELECT session_id, trigger_run_id, parent_checkpoint_id, source_entry_high_water,
                    prepared_entry_high_water, source_digest, instruction_digest, binding_digest,
                    maintenance_policy_version, state, prepared_sequence, prepared_at_milliseconds
             FROM compaction_maintenance_jobs WHERE job_id = ?1",
            [&id[..]],
            |row| {
                Ok((
                    row.get::<_, [u8; 16]>(0)?,
                    row.get::<_, [u8; 16]>(1)?,
                    row.get::<_, Option<[u8; 16]>>(2)?,
                    nonnegative_integer_from_row(row, 3)?,
                    nonnegative_integer_from_row(row, 4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    nonnegative_integer_from_row(row, 10)?,
                    nonnegative_integer_from_row(row, 11)?,
                ))
            },
        )?;
        let session = SessionId::from_bytes(row.0);
        let parent = row.2.map(|id| {
            connection.query_row(
                "SELECT source_entry_high_water, summary, estimated_summary_tokens FROM context_checkpoints
                 WHERE checkpoint_id = ?1 AND session_id = ?2",
                params![&id[..], &row.0[..]],
                |row| Ok(ContextCheckpoint { id: ContextCheckpointId::from_bytes(id),
                    source_entry_high_water: nonnegative_integer_from_row(row, 0)?, summary: row.get(1)?, estimated_summary_tokens: row.get(2)? }),
            ).map_err(PersistenceError::from)
        }).transpose()?;
        Ok(Self {
            id,
            session,
            run: load_required_run(connection, RunId::from_bytes(row.1))?,
            parent,
            source: row.3,
            through: row.4,
            source_digest: row.5,
            instruction_digest: row.6,
            binding_digest: row.7,
            policy: row.8,
            state: State::from_record(row.9)?,
            sequence: row.10,
            time: row.11,
        })
    }

    pub(super) fn digest(&self) -> [u8; 32] {
        let binding = serde_json::json!({
            "job": self.id, "session": self.session.as_bytes(), "trigger": self.run.id.as_bytes(),
            "service": self.run.service.to_record(), "model": self.run.model_id,
            "protocol": self.run.protocol_revision, "credential_generation": self.run.credential_generation,
            "context_policy": self.run.context_policy_version, "tools": self.run.tool_catalog_version,
            "limits": self.run.tool_limits_version, "maximum_input": self.run.maximum_input_tokens,
            "maximum_output": self.run.maximum_output_tokens, "trigger_source": self.run.source_entry_high_water,
            "parent": self.parent.as_ref().map(|parent| (parent.id.as_bytes(), parent.source_entry_high_water, &parent.summary)),
            "source": self.source, "through": self.through, "source_digest": self.source_digest,
            "instruction_digest": self.instruction_digest, "policy": self.policy,
            "sequence": self.sequence, "time": self.time,
        });
        hash_parts(
            b"morons.dev/maintenance-binding/v1\0",
            &[binding.to_string().as_bytes()],
        )
    }
}

pub(super) fn hash_parts(domain: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part);
    }
    digest.finalize().into()
}

pub(super) fn result_digest(id: &[u8; 16], payload: &str) -> [u8; 32] {
    hash_parts(
        b"morons.dev/maintenance-result/v1\0",
        &[id, payload.as_bytes()],
    )
}

pub(super) fn validate_result(payload: &str, run: &Run) -> Result<(), PersistenceError> {
    if payload.len() > MAX_RESULT_BYTES {
        return Err(invalid());
    }
    let value =
        crate::provider::json::parse_strict_value(payload.as_bytes()).map_err(|_| invalid())?;
    let result: MaintenanceResult = serde_json::from_value(value).map_err(|_| invalid())?;
    if result.summary.trim().is_empty()
        || result.summary.len() > super::super::context_budget::MAX_COMPACTION_SUMMARY_BYTES
        || result.summary.contains('\0')
        || result.input_tokens > u64::from(run.maximum_input_tokens)
        || result.output_tokens
            > u64::from(
                run.maximum_output_tokens
                    .min(crate::prompts::COMPACTION_OUTPUT_TOKENS),
            )
        || result.reasoning_output_tokens > result.output_tokens
        || result.input_tokens.checked_add(result.output_tokens) != Some(result.total_tokens)
        || result
            .cached_input_tokens
            .checked_add(result.cache_write_input_tokens)
            .is_none_or(|cached| cached > result.input_tokens)
    {
        return Err(invalid());
    }
    Ok(())
}

pub(super) fn validate_sequences(connection: &Connection) -> Result<(), PersistenceError> {
    const SEQUENCES: &str = "SELECT prepared_sequence AS sequence FROM compaction_maintenance_jobs UNION ALL SELECT fact_sequence FROM compaction_maintenance_events";
    let invalid_sequence: bool = connection.query_row(&format!(
        "SELECT EXISTS (SELECT 1 FROM ({SEQUENCES}) GROUP BY sequence HAVING COUNT(*) != 1
         OR sequence <= 0 OR sequence >= (SELECT next_value FROM logical_sequences WHERE singleton = 1))"
    ), [], |row| row.get(0))?;
    if invalid_sequence {
        return Err(invalid());
    }
    for (table, column) in [
        ("mutation_requests", "accepted_sequence"),
        ("deleted_mutation_tombstones", "accepted_sequence"),
        ("workspace_operation_facts", "fact_sequence"),
        ("session_created_facts", "fact_sequence"),
        ("audit_facts", "audit_sequence"),
        ("credential_operation_facts", "fact_sequence"),
        ("credential_audit_facts", "audit_sequence"),
        ("server_audit_facts", "audit_sequence"),
        ("session_entries", "fact_sequence"),
        ("run_accepted_facts", "fact_sequence"),
        ("run_state_facts", "fact_sequence"),
        ("run_cancellation_requests", "fact_sequence"),
        ("provider_operation_facts", "fact_sequence"),
        ("run_audit_facts", "audit_sequence"),
        ("worktree_generation_facts", "fact_sequence"),
        ("repository_import_facts", "fact_sequence"),
        ("repository_import_audit_facts", "audit_sequence"),
        ("tool_calls", "fact_sequence"),
        ("tool_operation_facts", "fact_sequence"),
        ("tool_uncertainty_acknowledgements", "fact_sequence"),
        ("tool_audit_facts", "audit_sequence"),
        ("execution_image_facts", "fact_sequence"),
        ("execution_image_audit_facts", "audit_sequence"),
        ("local_commands", "updated_sequence"),
        ("local_command_audit_facts", "audit_sequence"),
        ("context_checkpoints", "fact_sequence"),
        ("compaction_operations", "prepared_sequence"),
        ("compaction_operations", "updated_sequence"),
        ("workspace_generation_layouts", "created_sequence"),
        ("workspace_generation_layouts", "updated_sequence"),
        ("delivery_events", "event_sequence"),
    ] {
        let collision: bool = connection.query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM ({SEQUENCES}) AS maintenance
             WHERE EXISTS (SELECT 1 FROM {table} WHERE {column} = maintenance.sequence))"
            ),
            [],
            |row| row.get(0),
        )?;
        if collision {
            return Err(invalid());
        }
    }
    Ok(())
}

pub(super) fn latest_parent(
    connection: &Connection,
    session: SessionId,
) -> Result<Option<[u8; 16]>, PersistenceError> {
    connection.query_row(
        "SELECT checkpoint_id FROM context_checkpoints WHERE session_id = ?1 ORDER BY source_entry_high_water DESC LIMIT 1",
        [&session.as_bytes()[..]], |row| row.get(0),
    ).optional().map_err(PersistenceError::from)
}
