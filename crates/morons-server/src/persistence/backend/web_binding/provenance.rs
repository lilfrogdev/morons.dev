use super::{Backend, digest, invalid, nonnegative_integer_from_row, sequence_to_sql};
use crate::{
    persistence::{PersistenceError, WebBinding, WebInvocation},
    tools::{ToolInput, ToolOutput, ToolResult, WebSuccess},
};
use rusqlite::{Connection, OptionalExtension as _, params};
use sha2::{Digest as _, Sha256};

pub(super) fn required(
    connection: &Connection,
    binding: &WebBinding,
) -> Result<bool, PersistenceError> {
    let (epoch, next, first): (i64, i64, i64) = connection.query_row(
        "SELECT p.first_sequence,l.next_value,a.first_sequence FROM web_provenance_epoch p,logical_sequences l,web_attempt_epoch a WHERE p.singleton=1 AND l.singleton=1 AND a.singleton=1", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    if epoch < first || epoch > next {
        return Err(invalid());
    }
    Ok(sequence_to_sql(binding.sequence)? >= epoch)
}
fn binding_digest(binding: &WebBinding) -> [u8; 32] {
    Sha256::new()
        .chain_update(b"morons.dev/web-binding-provenance/v1\0")
        .chain_update(digest(binding, &[]))
        .chain_update([u8::from(binding.absence_generation.is_some())])
        .chain_update(binding.absence_generation.unwrap_or(0).to_be_bytes())
        .finalize()
        .into()
}
pub(super) fn insert_binding(
    connection: &Connection,
    binding: &WebBinding,
) -> Result<(), PersistenceError> {
    connection.execute("INSERT INTO web_binding_provenance(call_id,absence_generation,evidence_digest) VALUES(?1,?2,?3)", params![binding.call_id.as_bytes(), binding.absence_generation.map(sequence_to_sql).transpose()?, &binding_digest(binding)])?;
    Ok(())
}
pub(super) fn load_binding(
    connection: &Connection,
    binding: &mut WebBinding,
) -> Result<(), PersistenceError> {
    let evidence: Option<(Option<i64>, [u8;32])> = connection.query_row("SELECT absence_generation,evidence_digest FROM web_binding_provenance WHERE call_id=?1", [binding.call_id.as_bytes()], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((absence, stored)) = evidence else {
        return if required(connection, binding)? {
            Err(invalid())
        } else {
            Ok(())
        };
    };
    let absence = absence
        .map(|value| u64::try_from(value).map_err(|_| invalid()))
        .transpose()?;
    binding.absence_generation = absence;
    if stored != binding_digest(binding) || (binding.generation == 0) != absence.is_some() {
        return Err(invalid());
    }
    if let Some(generation) = absence {
        let historical: Option<(u64,bool)> = connection.query_row("SELECT result_generation,result_configured FROM credential_mutation_requests WHERE credential_kind=2 AND state=2 AND accepted_sequence<?1 ORDER BY result_generation DESC LIMIT 1", [sequence_to_sql(binding.sequence)?], |r| Ok((nonnegative_integer_from_row(r,0)?,r.get(1)?))).optional()?;
        if match historical {
            Some((g, configured)) => configured || g != generation,
            None => generation != 0,
        } {
            return Err(invalid());
        }
    }
    Ok(())
}
fn success_digest(admission: [u8; 32], payload: &[u8], sequence: u64) -> [u8; 32] {
    Sha256::new()
        .chain_update(b"morons.dev/web-search-success/v1\0")
        .chain_update(admission)
        .chain_update(sequence.to_be_bytes())
        .chain_update(payload)
        .finalize()
        .into()
}
fn decode(
    binding: &WebBinding,
    child: u16,
    ordinal: u64,
    payload: &[u8],
) -> Result<(WebSuccess, ToolResult), PersistenceError> {
    let result: ToolResult = serde_json::from_slice(payload).map_err(|_| invalid())?;
    let success =
        WebSuccess::new(binding.operation_id, child, ordinal, &result).ok_or_else(invalid)?;
    let query = match &result {
        ToolResult::Ok {
            output: ToolOutput::OpenAiWeb { result },
        } => &result.query,
        ToolResult::Ok {
            output: ToolOutput::ExaWeb { result },
        } => &result.query,
        _ => return Err(invalid()),
    };
    if !crate::tools::validate_canonical_result_for_input(
        &ToolInput::WebSearch {
            query: query.clone(),
        },
        &result,
    ) || serde_json::to_vec(&result).map_err(|_| invalid())? != payload
    {
        return Err(invalid());
    }
    Ok((success, result))
}
pub(super) fn successes(
    connection: &Connection,
    binding: &WebBinding,
) -> Result<Vec<(WebSuccess, ToolResult)>, PersistenceError> {
    let mut statement = connection.prepare("SELECT s.child,s.ordinal,s.result_payload,s.completion_sequence,s.success_digest,a.admission_digest,a.query_digest,a.route,a.dispatch_sequence FROM web_search_successes s JOIN web_search_attempts a USING(call_id,child,ordinal) WHERE s.call_id=?1 ORDER BY s.child,s.ordinal")?;
    let mut rows = statement.query([binding.call_id.as_bytes()])?;
    let mut results = Vec::new();
    while let Some(row) = rows.next()? {
        let payload: Vec<u8> = row.get(2)?;
        let sequence = nonnegative_integer_from_row(row, 3)?;
        let stored: [u8; 32] = row.get(4)?;
        let admission: [u8; 32] = row.get(5)?;
        let query: [u8; 32] = row.get(6)?;
        let route: i64 = row.get(7)?;
        let dispatch = nonnegative_integer_from_row(row, 8)?;
        let (success, result) = decode(
            binding,
            row.get(0)?,
            nonnegative_integer_from_row(row, 1)?,
            &payload,
        )?;
        let ordered: bool = connection.query_row("SELECT ?1<(SELECT next_value FROM logical_sequences WHERE singleton=1) AND NOT EXISTS(SELECT 1 FROM tool_operation_facts WHERE call_id=?2 AND fact_kind IN (3,4,5,6) AND fact_sequence<=?1)", params![sequence_to_sql(sequence)?,binding.call_id.as_bytes()], |r|r.get(0))?;
        if !ordered
            || sequence <= dispatch
            || query != success.query_digest
            || (route != 0) != success.exa
            || stored != success_digest(admission, &payload, sequence)
        {
            return Err(invalid());
        }
        results.push((success, result));
    }
    let expected: i64 = connection.query_row(
        "SELECT success_count FROM web_model_bindings WHERE call_id=?1",
        [binding.call_id.as_bytes()],
        |r| r.get(0),
    )?;
    if i64::try_from(results.len()).map_err(|_| invalid())? != expected {
        return Err(invalid());
    }
    Ok(results)
}
pub(super) fn validate_successes(
    connection: &Connection,
    binding: &WebBinding,
) -> Result<(), PersistenceError> {
    successes(connection, binding).map(|_| ())
}
pub(super) fn validate_result(
    connection: &Connection,
    binding: &WebBinding,
    result: &ToolResult,
) -> Result<(), PersistenceError> {
    let completed = successes(connection, binding)?;
    if !required(connection, binding)? && completed.is_empty() {
        return Ok(());
    }
    match result {
        ToolResult::Ok {
            output: ToolOutput::Task { results },
        } => {
            let mut matched = 0;
            for child in results {
                let successes: Vec<_> = completed
                    .iter()
                    .filter(|(s, _)| s.child == child.index)
                    .collect();
                let references: Vec<_> = successes.iter().map(|(s, _)| s.clone()).collect();
                let receipts: Vec<_> = successes
                    .iter()
                    .filter_map(|(_, r)| match r {
                        ToolResult::Ok {
                            output: ToolOutput::OpenAiWeb { result },
                        } => Some(result.receipt.clone()),
                        _ => None,
                    })
                    .collect();
                if child.web_successes != references
                    || child.web_searches != receipts
                    || child.exa_searches
                        != u64::try_from(successes.iter().filter(|(s, _)| s.exa).count())
                            .map_err(|_| invalid())?
                {
                    return Err(invalid());
                }
                matched += successes.len();
            }
            if matched != completed.len() {
                return Err(invalid());
            }
        }
        ToolResult::Ok {
            output: ToolOutput::OpenAiWeb { .. } | ToolOutput::ExaWeb { .. },
        } if completed.len() != 1 || completed[0].0.child != 0 || completed[0].1 != *result => {
            return Err(invalid());
        }
        _ => {}
    }
    Ok(())
}
impl Backend {
    pub(crate) fn complete_web_search(
        &mut self,
        binding: &WebBinding,
        invocation: &WebInvocation,
        result: &ToolResult,
    ) -> Result<WebSuccess, PersistenceError> {
        if self.load_web_binding(binding.run_id, binding.call_id)? != *binding {
            return Err(invalid());
        }
        let payload = serde_json::to_vec(result).map_err(|_| invalid())?;
        let (success, _) = decode(binding, invocation.child, invocation.ordinal, &payload)?;
        if success.query_digest != invocation.query_digest
            || success.exa != (invocation.route.record() != 0)
        {
            return Err(invalid());
        }
        let transaction = self.connection.transaction()?;
        let active: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE run_id=?1 AND state=2) AND NOT EXISTS(SELECT 1 FROM tool_operation_facts WHERE call_id=?2 AND fact_kind IN (3,4,5,6))", params![binding.run_id.as_bytes(),binding.call_id.as_bytes()], |r|r.get(0))?;
        let (admission,query,route): ([u8;32],[u8;32],i64) = transaction.query_row("SELECT admission_digest,query_digest,route FROM web_search_attempts WHERE call_id=?1 AND child=?2 AND ordinal=?3",params![binding.call_id.as_bytes(),invocation.child,sequence_to_sql(invocation.ordinal)?], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        if !active || query != success.query_digest || route != invocation.route.record() {
            return Err(invalid());
        }
        let sequence = super::super::records::next_sequence(&transaction)?;
        transaction.execute("INSERT INTO web_search_successes(call_id,child,ordinal,result_payload,completion_sequence,success_digest) VALUES(?1,?2,?3,?4,?5,?6)",params![binding.call_id.as_bytes(),invocation.child,sequence_to_sql(invocation.ordinal)?,&payload,sequence_to_sql(sequence)?,&success_digest(admission,&payload,sequence)])?;
        transaction.execute(
            "UPDATE web_model_bindings SET success_count=success_count+1 WHERE call_id=?1",
            [binding.call_id.as_bytes()],
        )?;
        transaction.commit()?;
        Ok(success)
    }
}
