mod attempts;
mod provenance;

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
    pub(crate) fn dispatch_web_search(
        &mut self,
        binding: &WebBinding,
        invocation: &crate::persistence::WebInvocation,
    ) -> Result<DataUsePolicy, PersistenceError> {
        let policy = self.admit_web_route(binding, invocation.route.record() != 0)?;
        if !attempts::valid_scope(binding, invocation) {
            return Err(invalid());
        }
        if matches!(invocation.route, crate::persistence::WebRoute::OpenAi)
            && self.require_model_credential(RunService::OpenAiChatGpt)? != binding.generation
        {
            return Err(PersistenceError::CredentialGenerationConflict);
        }
        let transaction = self.connection.transaction()?;
        attempts::validate_route(&transaction, binding, invocation.route)?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM web_search_attempts WHERE call_id=?1 AND child=?2 AND ordinal=?3)",
            params![binding.call_id.as_bytes(), invocation.child, sequence_to_sql(invocation.ordinal)?], |r| r.get(0),
        )?;
        if exists {
            return Err(PersistenceError::InvalidState {
                reason: "web search invocation was already admitted; nothing was retried",
            });
        }
        let sequence = super::records::next_sequence(&transaction)?;
        let digest = attempts::admission_digest(binding, invocation, policy.sequence, sequence);
        transaction.execute(
            "INSERT INTO web_search_attempts(call_id,child,ordinal,query_digest,route,contract_revision,policy_sequence,dispatch_sequence,admission_digest) VALUES(?1,?2,?3,?4,?5,1,?6,?7,?8)",
            params![binding.call_id.as_bytes(), invocation.child, sequence_to_sql(invocation.ordinal)?, &invocation.query_digest,
                invocation.route.record(), sequence_to_sql(policy.sequence)?, sequence_to_sql(sequence)?, &digest],
        )?;
        transaction.execute(
            "UPDATE web_model_bindings SET admission_count=admission_count+1 WHERE call_id=?1",
            [binding.call_id.as_bytes()],
        )?;
        transaction.commit()?;
        Ok(policy)
    }

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
        let mut absence_generation = None;
        let generation = if parent.service == RunService::OpenAiChatGpt {
            parent.credential_generation
        } else if let Some(task) = task.filter(|t| t.service == RunService::OpenAiChatGpt) {
            task.credential_generation
        } else {
            let status = self.openai_credential_status()?;
            if status.state != crate::persistence::OpenAiCredentialState::Unconfigured {
                status.generation
            } else {
                absence_generation = Some(status.generation);
                0
            }
        };
        let policy = self.data_use_policy()?;
        let exa_contract_revision = u16::from(crate::provider::exa::permits(policy.restrictions));
        if kind == ToolKind::WebSearch.to_record() && generation == 0 && exa_contract_revision == 0
        {
            return Err(PersistenceError::OpenAiCredentialNotConfigured);
        }
        let mut binding = WebBinding {
            run_id,
            call_id,
            operation_id: *operation.as_bytes(),
            generation,
            absence_generation,
            exa_contract_revision,
            policy_sequence: policy.sequence,
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
    pub(crate) fn admit_web_route(
        &mut self,
        expected: &WebBinding,
        exa: bool,
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
        if exa {
            let policy = self.data_use_policy()?;
            if binding.exa_contract_revision != 1
                || !crate::provider::exa::permits(policy.restrictions)
            {
                return Err(PersistenceError::DataUseRestricted);
            }
            return Ok(policy);
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
        let attempt_epoch: i64 = self.connection.query_row(
            "SELECT first_sequence FROM web_attempt_epoch WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        let provenance_epoch: i64 = self.connection.query_row(
            "SELECT first_sequence FROM web_provenance_epoch WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if provenance_epoch < attempt_epoch
            || provenance_epoch > next
            || attempt_epoch < epoch
            || attempt_epoch > next
        {
            return Err(invalid());
        }
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
    connection.execute("INSERT INTO web_model_bindings(call_id,run_id,operation_id,service,model_id,contract_revision,credential_kind,credential_generation,policy_sequence,dispatch_sequence,binding_digest,exa_contract_revision) VALUES(?1,?2,?3,3,?4,?5,2,?6,?7,?8,?9,?10)", params![binding.call_id.as_bytes(), binding.run_id.as_bytes(), &binding.operation_id, MODEL, CONTRACT_REVISION, sequence_to_sql(binding.generation)?,sequence_to_sql(binding.policy_sequence)?,sequence_to_sql(sequence)?,&digest,binding.exa_contract_revision])?;
    provenance::insert_binding(connection, &binding)?;
    Ok(())
}
fn load(connection: &Connection, call: ToolCallId) -> Result<WebBinding, PersistenceError> {
    let (mut binding, service, model, contract, kind, stored) = connection.query_row("SELECT run_id,operation_id,credential_generation,policy_sequence,dispatch_sequence,service,model_id,contract_revision,credential_kind,binding_digest,exa_contract_revision FROM web_model_bindings WHERE call_id=?1", [call.as_bytes()], |r| Ok((WebBinding { call_id: call, run_id: RunId::from_bytes(r.get(0)?), operation_id:r.get(1)?, generation:nonnegative_integer_from_row(r,2)?, absence_generation: None, policy_sequence:nonnegative_integer_from_row(r,3)?, sequence:nonnegative_integer_from_row(r,4)?, query_digest: None, children: 0, exa_contract_revision: r.get(10)? },r.get::<_,i64>(5)?,r.get::<_,String>(6)?,r.get::<_,u16>(7)?,r.get::<_,i64>(8)?,r.get::<_,[u8;32]>(9)?))).optional()?.ok_or_else(invalid)?;
    provenance::load_binding(connection, &mut binding)?;
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
        || binding.exa_contract_revision > 1
        || (tool == 12 && binding.generation == 0 && binding.exa_contract_revision == 0)
        || (parent.service == RunService::OpenAiChatGpt
            && binding.generation != parent.credential_generation)
    {
        return Err(invalid());
    }
    let exa_policy_allowed: bool = if binding.policy_sequence == 0 {
        true
    } else {
        connection.query_row("SELECT block_training_use=0 AND require_zero_retention=0 FROM data_use_policies WHERE accepted_sequence=?1", [sequence_to_sql(binding.policy_sequence)?], |r| r.get(0))?
    };
    if binding.exa_contract_revision != 0 && !exa_policy_allowed {
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
        attempts::validate_result(connection, &binding, &result)?;
        match result {
            crate::tools::ToolResult::Error {
                error: crate::tools::ToolErrorKind::WebSearchUncertain(failure),
                output,
            } => {
                let terminal: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM tool_operation_facts WHERE call_id=?1 AND fact_kind=6 AND result_status=4)", [call.as_bytes()], |r| r.get(0))?;
                if !diagnostic_allowed(connection, &binding, failure)?
                    || output.is_some()
                    || !terminal
                {
                    return Err(invalid());
                }
            }
            crate::tools::ToolResult::Error {
                error: crate::tools::ToolErrorKind::ExaSearchUncertain,
                output,
            } => {
                let terminal: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM tool_operation_facts WHERE call_id=?1 AND fact_kind=6 AND result_status=4)", [call.as_bytes()], |r| r.get(0))?;
                if binding.exa_contract_revision != 1 || output.is_some() || !terminal {
                    return Err(invalid());
                }
            }
            crate::tools::ToolResult::Ok {
                output: crate::tools::ToolOutput::ExaWeb { result },
            } => {
                if binding.exa_contract_revision != 1
                    || !result.is_valid()
                    || binding.query_digest != Some(Sha256::digest(result.query.as_bytes()).into())
                {
                    return Err(invalid());
                }
            }
            crate::tools::ToolResult::Ok {
                output: crate::tools::ToolOutput::OpenAiWeb { .. },
            } if binding.generation == 0 => return Err(invalid()),
            crate::tools::ToolResult::Ok {
                output: crate::tools::ToolOutput::WebSearch { .. },
            } => return Err(invalid()),
            crate::tools::ToolResult::Ok {
                output: crate::tools::ToolOutput::Task { results },
            } if results.iter().any(|r| {
                (binding.generation == 0 && !r.web_searches.is_empty())
                    || (binding.exa_contract_revision != 1 && r.exa_searches > 0)
            }) =>
            {
                return Err(invalid());
            }
            _ => {}
        }
    }
    provenance::validate_successes(connection, &binding)?;
    attempts::validate(connection, &binding)?;
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
    if binding.exa_contract_revision != 0 {
        hash.update(b"exa/keyless-mcp/web_search_exa/v1\0");
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
    let diagnostic = match result.error_kind() {
        Some(crate::tools::ToolErrorKind::WebSearchUncertain(failure)) => Some(failure),
        _ => None,
    };
    if !bound {
        return if diagnostic.is_some()
            || matches!(
                result,
                crate::tools::ToolResult::Error {
                    error: crate::tools::ToolErrorKind::ExaSearchUncertain,
                    ..
                } | crate::tools::ToolResult::Ok {
                    output: crate::tools::ToolOutput::ExaWeb { .. }
                }
            ) {
            Err(invalid())
        } else {
            Ok(())
        };
    }
    let binding = load(connection, call)?;
    attempts::validate_result(connection, &binding, result)?;
    if let Some(failure) = diagnostic
        && !diagnostic_allowed(connection, &binding, failure)?
    {
        return Err(invalid());
    }
    if matches!(
        result.error_kind(),
        Some(crate::tools::ToolErrorKind::ExaSearchUncertain)
    ) && binding.exa_contract_revision != 1
    {
        return Err(invalid());
    }
    match result {
        crate::tools::ToolResult::Ok {
            output: crate::tools::ToolOutput::ExaWeb { result },
        } => {
            if binding.exa_contract_revision != 1
                || !result.is_valid()
                || binding.query_digest != Some(Sha256::digest(result.query.as_bytes()).into())
            {
                return Err(invalid());
            }
        }
        crate::tools::ToolResult::Ok {
            output: crate::tools::ToolOutput::OpenAiWeb { result },
        } => {
            if binding.generation == 0
                || !result.is_valid()
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
            (r.exa_searches > 0 && binding.exa_contract_revision != 1)
                || (!r.web_searches.is_empty()
                    && (binding.generation == 0
                        || !u64::try_from(r.web_searches.len())
                            .is_ok_and(|count| count <= r.tool_calls)
                        || r.web_searches.iter().any(|w| !w.is_valid())))
        }) =>
        {
            return Err(invalid());
        }
        _ => {}
    }
    Ok(())
}
fn diagnostic_allowed(
    connection: &Connection,
    binding: &WebBinding,
    failure: crate::web_diagnostic::WebFailure,
) -> Result<bool, PersistenceError> {
    if binding.generation == 0 {
        return Ok(false);
    }
    // Canonical acceptance, not a cached/derived run projection, owns this result vocabulary.
    let (catalog, limits): (u16, u16) = connection.query_row(
        "SELECT tool_catalog_version,tool_limits_version FROM run_accepted_facts WHERE run_id=?1",
        [binding.run_id.as_bytes()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok((catalog == limits || (catalog, limits) == (13, 14)) && failure.valid_for_catalog(catalog))
}
fn invalid() -> PersistenceError {
    PersistenceError::InvalidState {
        reason: "web model binding is invalid or inconsistent",
    }
}
