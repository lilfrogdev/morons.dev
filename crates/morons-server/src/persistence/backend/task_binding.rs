use super::{
    Backend,
    records::{nonnegative_integer_from_row, sequence_to_sql},
    run_records::load_required_run,
};
use crate::{
    persistence::{
        PersistenceError, RunId, RunService, SubagentModelSetting, TaskModelBinding, ToolCallId,
        run_types::ToolOperationId,
    },
    tools::ToolKind,
};
use rusqlite::{Connection, OptionalExtension as _, params};
use sha2::{Digest as _, Sha256};

impl Backend {
    pub(super) fn task_binding_candidate(
        &mut self,
        run_id: RunId,
        call_id: ToolCallId,
        operation_id: ToolOperationId,
    ) -> Result<Option<TaskModelBinding>, PersistenceError> {
        self.ensure_context_integrity()?;
        let kind: i64 = self.connection.query_row(
            "SELECT tool_kind FROM tool_calls WHERE run_id=?1 AND call_id=?2 AND operation_id=?3",
            params![
                &run_id.as_bytes()[..],
                &call_id.as_bytes()[..],
                &operation_id.as_bytes()[..]
            ],
            |row| row.get(0),
        )?;
        if kind != ToolKind::Task.to_record() {
            return Ok(None);
        }
        let parent = load_required_run(&self.connection, run_id)?;
        let (service, model_id, inherit) = match self.subagent_model_setting()? {
            SubagentModelSetting::InheritParent {} => {
                (parent.service, parent.model_id.clone(), true)
            }
            SubagentModelSetting::Explicit { service, model_id } => (service, model_id, false),
        };
        let model = crate::provider::find_model_profile(service.model_service(), &model_id)
            .filter(|m| {
                m.capabilities.tool_calls && m.capabilities.text_input && m.capabilities.text_output
            })
            .ok_or(PersistenceError::InvalidInput {
                reason: "the selected task model is unavailable",
            })?;
        let credential_generation = if service.credential_kind() == parent.service.credential_kind()
        {
            parent.credential_generation
        } else {
            self.require_model_credential(service)?
        };
        Ok(Some(TaskModelBinding {
            run_id,
            call_id,
            operation_id: *operation_id.as_bytes(),
            service,
            model_id,
            credential_generation,
            protocol_revision: if inherit {
                parent.protocol_revision
            } else {
                model.protocol_revision
            },
            maximum_input_tokens: if inherit {
                parent.maximum_input_tokens
            } else {
                model.maximum_input_tokens
            },
            maximum_output_tokens: if inherit {
                parent.maximum_output_tokens
            } else {
                model.maximum_output_tokens
            },
            policy_sequence: self.data_use_policy()?.sequence,
            sequence: 0,
        }))
    }
    pub(crate) fn load_task_model_binding(
        &self,
        run_id: RunId,
        call_id: ToolCallId,
    ) -> Result<TaskModelBinding, PersistenceError> {
        self.ensure_context_integrity()?;
        let binding = load(&self.connection, call_id)?;
        if binding.run_id != run_id {
            return Err(invalid());
        }
        Ok(binding)
    }
    pub(super) fn validate_task_bindings(&self) -> Result<(), PersistenceError> {
        let epoch: i64 = self.connection.query_row(
            "SELECT first_sequence FROM provider_binding_epoch WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        let next: i64 = self.connection.query_row(
            "SELECT next_value FROM logical_sequences WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        if epoch < 1 || epoch > next {
            return Err(invalid());
        }
        let missing:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM tool_operation_facts AS operation JOIN tool_calls AS call USING(call_id) LEFT JOIN task_model_bindings AS binding USING(call_id) WHERE operation.fact_kind=2 AND operation.fact_sequence>=?1 AND call.tool_kind=?2 AND binding.call_id IS NULL)",params![epoch,ToolKind::Task.to_record()],|row|row.get(0))?;
        let count: i64 =
            self.connection
                .query_row("SELECT COUNT(*) FROM task_model_bindings", [], |row| {
                    row.get(0)
                })?;
        if missing || count > 100_000 * i64::from(crate::tools::MAX_TASK_CALLS_PER_RUN) {
            return Err(invalid());
        }
        let mut after = 0_i64;
        let mut visited = 0;
        loop {
            let mut statement=self.connection.prepare("SELECT call_id,dispatch_sequence FROM task_model_bindings WHERE dispatch_sequence>?1 ORDER BY dispatch_sequence LIMIT 32")?;
            let rows = statement
                .query_map([after], |row| {
                    Ok((row.get::<_, [u8; 16]>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            if rows.is_empty() {
                break;
            }
            for (call, sequence) in rows {
                if sequence < epoch || sequence >= next {
                    return Err(invalid());
                }
                load(&self.connection, ToolCallId::from_bytes(call))?;
                after = sequence;
                visited += 1;
            }
        }
        if visited != count {
            return Err(invalid());
        }
        Ok(())
    }
}

pub(super) fn insert(
    connection: &Connection,
    mut binding: TaskModelBinding,
    sequence: u64,
) -> Result<(), PersistenceError> {
    binding.sequence = sequence;
    let input = source(connection, &binding)?;
    let digest = digest(&binding, &input);
    connection.execute("INSERT INTO task_model_bindings (call_id,run_id,operation_id,service,model_id,credential_kind,credential_generation,protocol_revision,maximum_input_tokens,maximum_output_tokens,policy_sequence,dispatch_sequence,binding_digest) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",params![&binding.call_id.as_bytes()[..],&binding.run_id.as_bytes()[..],&binding.operation_id[..],binding.service.to_record(),&binding.model_id,(binding.service.credential_kind() as i64),sequence_to_sql(binding.credential_generation)?,binding.protocol_revision,binding.maximum_input_tokens,binding.maximum_output_tokens,sequence_to_sql(binding.policy_sequence)?,sequence_to_sql(sequence)?,&digest[..]])?;
    Ok(())
}
fn load(
    connection: &Connection,
    call_id: ToolCallId,
) -> Result<TaskModelBinding, PersistenceError> {
    let (binding,kind,stored)=connection.query_row("SELECT run_id,operation_id,service,model_id,credential_generation,protocol_revision,maximum_input_tokens,maximum_output_tokens,policy_sequence,dispatch_sequence,credential_kind,binding_digest FROM task_model_bindings WHERE call_id=?1",[&call_id.as_bytes()[..]],|row|Ok((TaskModelBinding {call_id,run_id:RunId::from_bytes(row.get(0)?),operation_id:row.get(1)?,service:RunService::from_record(row.get(2)?)?,model_id:row.get(3)?,credential_generation:nonnegative_integer_from_row(row,4)?,protocol_revision:row.get(5)?,maximum_input_tokens:row.get(6)?,maximum_output_tokens:row.get(7)?,policy_sequence:nonnegative_integer_from_row(row,8)?,sequence:nonnegative_integer_from_row(row,9)?},row.get::<_,i64>(10)?,row.get::<_,[u8;32]>(11)?))).optional()?.ok_or_else(invalid)?;
    let input = source(connection, &binding)?;
    if kind != (binding.service.credential_kind() as i64) || digest(&binding, &input) != stored {
        return Err(invalid());
    }
    let dispatched:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM tool_operation_facts WHERE call_id=?1 AND run_id=?2 AND operation_id=?3 AND fact_kind=2 AND fact_sequence=?4)",params![&call_id.as_bytes()[..],&binding.run_id.as_bytes()[..],&binding.operation_id[..],sequence_to_sql(binding.sequence)?],|row|row.get(0))?;
    let policy:u64=connection.query_row("SELECT COALESCE(MAX(accepted_sequence),0) FROM data_use_policies WHERE accepted_sequence<?1",[sequence_to_sql(binding.sequence)?],|row|nonnegative_integer_from_row(row,0))?;
    let parent = load_required_run(connection, binding.run_id)?;
    if !dispatched
        || policy != binding.policy_sequence
        || (parent.service.credential_kind() == binding.service.credential_kind()
            && parent.credential_generation != binding.credential_generation)
    {
        return Err(invalid());
    }
    let result: Option<Vec<u8>> = connection.query_row(
        "SELECT result_payload FROM tool_operation_facts WHERE call_id=?1 AND fact_kind IN (3,4,5,6)",
        [&call_id.as_bytes()[..]], |row| row.get(0),
    ).optional()?;
    if let Some(payload) = result {
        let result: crate::tools::ToolResult =
            serde_json::from_slice(&payload).map_err(|_| invalid())?;
        if let crate::tools::ToolResult::Ok {
            output: crate::tools::ToolOutput::Task { results },
        } = result
            && results.iter().any(|result| {
                result.model.as_ref().is_none_or(|model| {
                    model.service != binding.service.model_service().label()
                        || model.model_id != binding.model_id
                        || model.protocol_revision != binding.protocol_revision
                })
            })
        {
            return Err(invalid());
        }
    }
    Ok(binding)
}
fn source(
    connection: &Connection,
    binding: &TaskModelBinding,
) -> Result<Vec<u8>, PersistenceError> {
    connection.query_row("SELECT input_payload FROM tool_calls WHERE call_id=?1 AND run_id=?2 AND operation_id=?3 AND tool_kind=?4",params![&binding.call_id.as_bytes()[..],&binding.run_id.as_bytes()[..],&binding.operation_id[..],ToolKind::Task.to_record()],|row|row.get(0)).optional()?.ok_or_else(invalid)
}
fn digest(binding: &TaskModelBinding, input: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"morons.dev/task-model-binding/v1\0");
    let metadata = serde_json::json!([
        binding.call_id.as_bytes(),
        binding.run_id.as_bytes(),
        binding.operation_id,
        binding.service.to_record(),
        binding.model_id,
        (binding.service.credential_kind() as i64),
        binding.credential_generation,
        binding.protocol_revision,
        binding.maximum_input_tokens,
        binding.maximum_output_tokens,
        binding.policy_sequence,
        binding.sequence
    ])
    .to_string();
    for bytes in [metadata.as_bytes(), input] {
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    hash.finalize().into()
}
fn invalid() -> PersistenceError {
    PersistenceError::InvalidState {
        reason: "task model binding is invalid or inconsistent",
    }
}
