use super::{
    Backend,
    records::{nonnegative_integer_from_row, sequence_to_sql},
    run_records::load_required_run,
};
use crate::{
    persistence::{
        DataUsePolicy, PersistenceError, RunId, RunService, TaskModelBinding, ToolCallId,
        WebBinding, run_types::ToolOperationId,
    },
    provider::openai_web::{CONTRACT_REVISION, MAX_RESPONSE_BYTES, MODEL},
    tools::ToolKind,
};
use rusqlite::{Connection, OptionalExtension as _, params};
use sha2::{Digest as _, Sha256};

impl Backend {
    pub(super) fn web_binding_candidate(
        &mut self,
        run_id: RunId,
        call_id: ToolCallId,
        operation: ToolOperationId,
        task: Option<&TaskModelBinding>,
    ) -> Result<Option<WebBinding>, PersistenceError> {
        let kind: i64 = self.connection.query_row(
            "SELECT tool_kind FROM tool_calls WHERE run_id=?1 AND call_id=?2 AND operation_id=?3",
            params![run_id.as_bytes(), call_id.as_bytes(), operation.as_bytes()],
            |row| row.get(0),
        )?;
        if !matches!(
            ToolKind::from_record(kind),
            Some(ToolKind::WebSearch | ToolKind::Task)
        ) {
            return Ok(None);
        }
        let parent = load_required_run(&self.connection, run_id)?;
        let generation = if parent.service == RunService::OpenAiChatGpt {
            parent.credential_generation
        } else if let Some(task) = task.filter(|t| t.service == RunService::OpenAiChatGpt) {
            task.credential_generation
        } else {
            let status = self.model_credential_status(RunService::OpenAiChatGpt)?;
            if status.configured {
                status.generation
            } else {
                0
            }
        };
        if kind == ToolKind::WebSearch.to_record() && generation == 0 {
            return Err(PersistenceError::OpenAiCredentialNotConfigured);
        }
        let mut binding = WebBinding {
            run_id,
            call_id,
            operation_id: *operation.as_bytes(),
            generation,
            policy_sequence: self.data_use_policy()?.sequence,
            sequence: 0,
            query_digest: None,
            children: 0,
        };
        let input = source(&self.connection, &binding)?.1;
        bind_source(&mut binding, &input)?;
        Ok(Some(binding))
    }
    pub(crate) fn load_web_binding(
        &self,
        run: RunId,
        call: ToolCallId,
    ) -> Result<WebBinding, PersistenceError> {
        self.ensure_context_integrity()?;
        let binding = load(&self.connection, call)?;
        if binding.run_id != run {
            return Err(invalid());
        }
        Ok(binding)
    }
    pub(crate) fn admit_web_binding(
        &mut self,
        expected: &WebBinding,
    ) -> Result<DataUsePolicy, PersistenceError> {
        let binding = self.load_web_binding(expected.run_id, expected.call_id)?;
        if binding != *expected {
            return Err(invalid());
        }
        let active: bool = self.connection.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE run_id=?1 AND state=2 AND cancellation_requested=0)", [binding.run_id.as_bytes()], |row| row.get(0))?;
        let terminal: bool = self.connection.query_row("SELECT EXISTS(SELECT 1 FROM tool_operation_facts WHERE call_id=?1 AND fact_kind IN (3,4,5,6))", [binding.call_id.as_bytes()], |row| row.get(0))?;
        if !active || terminal {
            return Err(PersistenceError::InvalidInput {
                reason: "web search owner is no longer active",
            });
        }
        if binding.generation == 0 {
            return Err(PersistenceError::OpenAiCredentialNotConfigured);
        }
        self.admit_model_data_use(RunService::OpenAiChatGpt, MODEL)
    }
    pub(super) fn validate_web_bindings(&self) -> Result<(), PersistenceError> {
        let epoch: i64 = self.connection.query_row(
            "SELECT first_sequence FROM web_binding_epoch WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        let next: i64 = self.connection.query_row(
            "SELECT next_value FROM logical_sequences WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        let missing: bool = self.connection.query_row("SELECT EXISTS(SELECT 1 FROM tool_operation_facts AS fact JOIN tool_calls AS call USING(call_id) LEFT JOIN web_model_bindings AS binding USING(call_id) WHERE fact.fact_kind=2 AND fact.fact_sequence>=?1 AND call.tool_kind IN (12,14) AND binding.call_id IS NULL)", [epoch], |r| r.get(0))?;
        let count: i64 =
            self.connection
                .query_row("SELECT COUNT(*) FROM web_model_bindings", [], |r| r.get(0))?;
        if epoch < 1 || epoch > next || missing || count > 6_400_000 {
            return Err(invalid());
        }
        let mut after = 0_i64;
        let mut visited = 0;
        loop {
            let mut statement = self.connection.prepare("SELECT call_id,dispatch_sequence FROM web_model_bindings WHERE dispatch_sequence>?1 ORDER BY dispatch_sequence LIMIT 32")?;
            let rows = statement
                .query_map([after], |r| {
                    Ok((r.get::<_, [u8; 16]>(0)?, r.get::<_, i64>(1)?))
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
    mut binding: WebBinding,
    sequence: u64,
) -> Result<(), PersistenceError> {
    binding.sequence = sequence;
    let (_, input) = source(connection, &binding)?;
    let digest = digest(&binding, &input);
    connection.execute("INSERT INTO web_model_bindings(call_id,run_id,operation_id,service,model_id,contract_revision,credential_kind,credential_generation,policy_sequence,dispatch_sequence,binding_digest) VALUES(?1,?2,?3,3,?4,?5,2,?6,?7,?8,?9)", params![binding.call_id.as_bytes(), binding.run_id.as_bytes(), &binding.operation_id, MODEL, CONTRACT_REVISION, sequence_to_sql(binding.generation)?,sequence_to_sql(binding.policy_sequence)?,sequence_to_sql(sequence)?,&digest])?;
    Ok(())
}
fn load(connection: &Connection, call: ToolCallId) -> Result<WebBinding, PersistenceError> {
    let (mut binding, service, model, contract, kind, stored) = connection.query_row("SELECT run_id,operation_id,credential_generation,policy_sequence,dispatch_sequence,service,model_id,contract_revision,credential_kind,binding_digest FROM web_model_bindings WHERE call_id=?1", [call.as_bytes()], |r| Ok((WebBinding { call_id: call, run_id: RunId::from_bytes(r.get(0)?), operation_id:r.get(1)?, generation:nonnegative_integer_from_row(r,2)?, policy_sequence:nonnegative_integer_from_row(r,3)?, sequence:nonnegative_integer_from_row(r,4)?, query_digest: None, children: 0 },r.get::<_,i64>(5)?,r.get::<_,String>(6)?,r.get::<_,u16>(7)?,r.get::<_,i64>(8)?,r.get::<_,[u8;32]>(9)?))).optional()?.ok_or_else(invalid)?;
    let (tool, input) = source(connection, &binding)?;
    bind_source(&mut binding, &input)?;
    let dispatched: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM tool_operation_facts WHERE run_id=?1 AND call_id=?2 AND operation_id=?3 AND fact_kind=2 AND fact_sequence=?4)", params![binding.run_id.as_bytes(), call.as_bytes(), &binding.operation_id, sequence_to_sql(binding.sequence)?], |r| r.get(0))?;
    let policy: u64 = connection.query_row("SELECT COALESCE(MAX(accepted_sequence),0) FROM data_use_policies WHERE accepted_sequence<?1", [sequence_to_sql(binding.sequence)?], |r| nonnegative_integer_from_row(r,0))?;
    let parent = load_required_run(connection, binding.run_id)?;
    if service != 3
        || model != MODEL
        || contract != CONTRACT_REVISION
        || kind != 2
        || stored != digest(&binding, &input)
        || !dispatched
        || policy != binding.policy_sequence
        || (tool == 12 && binding.generation == 0)
        || (parent.service == RunService::OpenAiChatGpt
            && binding.generation != parent.credential_generation)
    {
        return Err(invalid());
    }
    if tool == 14 {
        let child: (i64, u64) = connection.query_row(
            "SELECT service,credential_generation FROM task_model_bindings WHERE call_id=?1",
            [call.as_bytes()],
            |r| Ok((r.get(0)?, nonnegative_integer_from_row(r, 1)?)),
        )?;
        if child.0 == 3 && child.1 != binding.generation {
            return Err(invalid());
        }
    }
    if parent.service != RunService::OpenAiChatGpt && binding.generation > 0 {
        let recorded: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM credential_mutation_requests WHERE credential_kind=2 AND state=2 AND result_generation=?1 AND result_configured=1 AND accepted_sequence<?2 AND NOT EXISTS(SELECT 1 FROM credential_mutation_requests AS newer WHERE newer.credential_kind=2 AND newer.state=2 AND newer.accepted_sequence<?2 AND newer.result_generation>?1))",params![sequence_to_sql(binding.generation)?,sequence_to_sql(binding.sequence)?],|r|r.get(0))?;
        if !recorded {
            return Err(invalid());
        }
    }
    let result: Option<Vec<u8>> = connection.query_row("SELECT CASE WHEN length(result_payload)<=524288 THEN result_payload ELSE X'' END FROM tool_operation_facts WHERE call_id=?1 AND fact_kind IN (3,4,5,6) AND result_payload IS NOT NULL", [call.as_bytes()], |r|r.get(0)).optional()?;
    if let Some(bytes) = result {
        let result: crate::tools::ToolResult =
            serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        match result {
            crate::tools::ToolResult::Ok {
                output: crate::tools::ToolOutput::WebSearch { .. },
            } => return Err(invalid()),
            crate::tools::ToolResult::Ok {
                output: crate::tools::ToolOutput::Task { results },
            } if binding.generation == 0 && results.iter().any(|r| !r.web_searches.is_empty()) => {
                return Err(invalid());
            }
            _ => {}
        }
    }
    Ok(binding)
}
fn source(
    connection: &Connection,
    binding: &WebBinding,
) -> Result<(i64, Vec<u8>), PersistenceError> {
    connection.query_row("SELECT tool_kind,CASE WHEN length(input_payload)<=524288 THEN input_payload ELSE X'' END FROM tool_calls WHERE call_id=?1 AND run_id=?2 AND operation_id=?3 AND tool_kind IN (12,14)", params![binding.call_id.as_bytes(),binding.run_id.as_bytes(),&binding.operation_id], |r| Ok((r.get(0)?,r.get(1)?))).optional()?.ok_or_else(invalid)
}
fn bind_source(binding: &mut WebBinding, input: &[u8]) -> Result<(), PersistenceError> {
    let input: crate::tools::ToolInput = serde_json::from_slice(input).map_err(|_| invalid())?;
    if !crate::tools::validate_canonical_input(&input) {
        return Err(invalid());
    }
    match input {
        crate::tools::ToolInput::WebSearch { query } => {
            binding.query_digest = Some(Sha256::digest(query.as_bytes()).into())
        }
        crate::tools::ToolInput::Task { tasks, .. } => {
            binding.children = u16::try_from(tasks.len()).map_err(|_| invalid())?
        }
        _ => return Err(invalid()),
    }
    Ok(())
}
fn digest(binding: &WebBinding, input: &[u8]) -> [u8; 32] {
    let metadata = serde_json::json!([
        binding.run_id.as_bytes(),
        binding.call_id.as_bytes(),
        binding.operation_id,
        3,
        MODEL,
        CONTRACT_REVISION,
        2,
        binding.generation,
        binding.policy_sequence,
        binding.sequence,
        MAX_RESPONSE_BYTES,
        crate::tools::MAX_WEB_SEARCH_QUERY_BYTES
    ])
    .to_string();
    let mut hash = Sha256::new().chain_update(b"morons.dev/web-model-binding/v1\0");
    for value in [metadata.as_bytes(), input] {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value);
    }
    hash.finalize().into()
}
pub(super) fn validate_result(
    connection: &Connection,
    call: ToolCallId,
    result: &crate::tools::ToolResult,
) -> Result<(), PersistenceError> {
    let bound: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM web_model_bindings WHERE call_id=?1)",
        [call.as_bytes()],
        |r| r.get(0),
    )?;
    if !bound {
        return Ok(());
    }
    let binding = load(connection, call)?;
    match result {
        crate::tools::ToolResult::Ok {
            output: crate::tools::ToolOutput::OpenAiWeb { result },
        } => {
            if !result.is_valid()
                || binding.query_digest != Some(Sha256::digest(result.query.as_bytes()).into())
            {
                return Err(invalid());
            }
        }
        crate::tools::ToolResult::Ok {
            output: crate::tools::ToolOutput::WebSearch { .. },
        } => return Err(invalid()),
        crate::tools::ToolResult::Ok {
            output: crate::tools::ToolOutput::Task { results },
        } if results.iter().any(|r| {
            !r.web_searches.is_empty()
                && (binding.generation == 0
                    || r.web_searches.len() > usize::from(r.tool_calls)
                    || r.web_searches.iter().any(|w| !w.is_valid()))
        }) =>
        {
            return Err(invalid());
        }
        _ => {}
    }
    Ok(())
}
fn invalid() -> PersistenceError {
    PersistenceError::InvalidState {
        reason: "web model binding is invalid or inconsistent",
    }
}
