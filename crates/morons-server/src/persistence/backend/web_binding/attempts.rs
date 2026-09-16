use super::{digest, invalid, nonnegative_integer_from_row, sequence_to_sql};
use crate::persistence::{PersistenceError, WebBinding, WebInvocation, WebRoute};
use rusqlite::{Connection, OptionalExtension as _, params};
use sha2::{Digest as _, Sha256};

pub(super) fn admission_digest(
    binding: &WebBinding,
    invocation: &WebInvocation,
    policy: u64,
    sequence: u64,
) -> [u8; 32] {
    // Keep historical ordinal encodings unchanged; wider ordinals use a separate domain.
    let (domain, ordinal): (&[u8], Vec<u8>) = match u16::try_from(invocation.ordinal) {
        Ok(ordinal) => (
            b"morons.dev/web-search-admission/v1\0",
            ordinal.to_be_bytes().to_vec(),
        ),
        Err(_) => (
            b"morons.dev/web-search-admission/v2\0",
            invocation.ordinal.to_be_bytes().to_vec(),
        ),
    };
    Sha256::new()
        .chain_update(domain)
        .chain_update(digest(binding, &[]))
        .chain_update(invocation.child.to_be_bytes())
        .chain_update(ordinal)
        .chain_update(invocation.query_digest)
        .chain_update(invocation.route.record().to_be_bytes())
        .chain_update(policy.to_be_bytes())
        .chain_update(sequence.to_be_bytes())
        .finalize()
        .into()
}

pub(super) fn valid_scope(binding: &WebBinding, invocation: &WebInvocation) -> bool {
    if invocation.child == 0 {
        invocation.ordinal == 0
            && binding.children == 0
            && binding.query_digest == Some(invocation.query_digest)
    } else {
        binding.query_digest.is_none()
            && invocation.child <= binding.children
            && invocation.ordinal > 0
    }
}

pub(super) fn validate_route(
    connection: &Connection,
    binding: &WebBinding,
    route: WebRoute,
) -> Result<(), PersistenceError> {
    match route {
        WebRoute::OpenAi if binding.generation > 0 => Ok(()),
        WebRoute::ExaMissingCredential if binding.generation == 0 => {
            if super::provenance::required(connection, binding)?
                && binding.absence_generation.is_none()
            {
                return Err(invalid());
            }
            let configured: bool = connection.query_row(
                "SELECT COALESCE((SELECT result_configured FROM credential_mutation_requests WHERE credential_kind=2 AND state=2 AND accepted_sequence<?1 ORDER BY result_generation DESC LIMIT 1),0)",
                [sequence_to_sql(binding.sequence)?], |r| r.get(0),
            )?;
            if configured { Err(invalid()) } else { Ok(()) }
        }
        // Exa's unrestricted policy also permits OpenAI, so policy denial cannot justify it.
        _ => Err(invalid()),
    }
}

pub(super) fn validate(
    connection: &Connection,
    binding: &WebBinding,
) -> Result<(), PersistenceError> {
    let expected_count: i64 = connection.query_row(
        "SELECT admission_count FROM web_model_bindings WHERE call_id=?1",
        [binding.call_id.as_bytes()],
        |row| row.get(0),
    )?;
    let mut statement = connection.prepare(
        "SELECT child,ordinal,query_digest,route,policy_sequence,dispatch_sequence,admission_digest FROM web_search_attempts WHERE call_id=?1"
    )?;
    let mut rows = statement.query([binding.call_id.as_bytes()])?;
    let mut count = 0_i64;
    while let Some(row) = rows.next()? {
        count = count.checked_add(1).ok_or_else(invalid)?;
        let route = match row.get::<_, i64>(3)? {
            0 => WebRoute::OpenAi,
            1 => WebRoute::ExaMissingCredential,
            2 => WebRoute::ExaPolicyDenied,
            _ => return Err(invalid()),
        };
        validate_route(connection, binding, route)?;
        let invocation = WebInvocation {
            child: row.get(0)?,
            ordinal: nonnegative_integer_from_row(row, 1)?,
            query_digest: row.get(2)?,
            route,
        };
        let policy = nonnegative_integer_from_row(row, 4)?;
        let sequence = nonnegative_integer_from_row(row, 5)?;
        let stored: [u8; 32] = row.get(6)?;
        let policy_valid: bool = connection.query_row(
            "SELECT ?1=COALESCE((SELECT MAX(accepted_sequence) FROM data_use_policies WHERE accepted_sequence<?2),0) AND ?2<(SELECT next_value FROM logical_sequences WHERE singleton=1) AND NOT EXISTS(SELECT 1 FROM tool_operation_facts WHERE call_id=?3 AND fact_kind IN (3,4,5,6) AND fact_sequence<=?2)",
            params![sequence_to_sql(policy)?, sequence_to_sql(sequence)?, binding.call_id.as_bytes()], |r| r.get(0),
        )?;
        if !valid_scope(binding, &invocation)
            || !policy_valid
            || sequence <= binding.sequence
            || sequence <= policy
            || stored != admission_digest(binding, &invocation, policy, sequence)
            || (route.record() == 0 && binding.generation == 0)
            || (route.record() != 0 && binding.exa_contract_revision != 1)
        {
            return Err(invalid());
        }
        if route.record() != 0 && policy != 0 {
            let permitted: bool = connection.query_row(
                "SELECT block_training_use=0 AND require_zero_retention=0 FROM data_use_policies WHERE accepted_sequence=?1",
                [sequence_to_sql(policy)?], |r| r.get(0),
            )?;
            if !permitted {
                return Err(invalid());
            }
        }
    }
    if count != expected_count {
        return Err(invalid());
    }
    Ok(())
}

pub(super) fn validate_result(
    connection: &Connection,
    binding: &WebBinding,
    result: &crate::tools::ToolResult,
) -> Result<(), PersistenceError> {
    use crate::tools::{ToolErrorKind, ToolOutput, ToolResult};
    super::provenance::validate_result(connection, binding, result)?;
    let epoch: i64 = connection.query_row(
        "SELECT first_sequence FROM web_attempt_epoch WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    if binding.query_digest.is_none() {
        if sequence_to_sql(binding.sequence)? >= epoch
            && let ToolResult::Ok {
                output: ToolOutput::Task { results },
            } = result
        {
            for child in results {
                let (native, exa, ordinal): (u64, u64, u64) = connection.query_row(
                    "SELECT COUNT(CASE WHEN route=0 THEN 1 END),COUNT(CASE WHEN route<>0 THEN 1 END),COALESCE(MAX(ordinal),0) FROM web_search_attempts WHERE call_id=?1 AND child=?2",
                    params![binding.call_id.as_bytes(), child.index],
                    |r| Ok((nonnegative_integer_from_row(r, 0)?, nonnegative_integer_from_row(r, 1)?, nonnegative_integer_from_row(r, 2)?)),
                )?;
                // Failed searches consume admissions without producing successful receipts.
                if u64::try_from(child.web_searches.len()).map_err(|_| invalid())? > native
                    || child.exa_searches > exa
                    || ordinal > child.tool_calls
                {
                    return Err(invalid());
                }
            }
        }
        if !matches!(
            result,
            ToolResult::Error {
                error: ToolErrorKind::WebSearchUncertain(_) | ToolErrorKind::ExaSearchUncertain,
                ..
            }
        ) {
            return Ok(());
        }
    }
    let exa = match result {
        ToolResult::Ok {
            output: ToolOutput::ExaWeb { .. },
        }
        | ToolResult::Error {
            error: ToolErrorKind::ExaSearchUncertain,
            ..
        } => true,
        ToolResult::Ok {
            output: ToolOutput::OpenAiWeb { .. },
        }
        | ToolResult::Error {
            error: ToolErrorKind::WebSearchUncertain(_),
            ..
        } => false,
        _ => return Ok(()),
    };
    if binding.query_digest.is_none() {
        let (total, matching): (u64, u64) = connection.query_row(
            "SELECT COUNT(*),COUNT(CASE WHEN (route<>0)=?2 THEN 1 END) FROM web_search_attempts WHERE call_id=?1 AND child>0",
            params![binding.call_id.as_bytes(), exa],
            |r| Ok((nonnegative_integer_from_row(r, 0)?, nonnegative_integer_from_row(r, 1)?)),
        )?;
        // Historical Tasks may lack admissions, but existing evidence must match the provider.
        if matching == 0 && (total > 0 || sequence_to_sql(binding.sequence)? >= epoch) {
            return Err(invalid());
        }
        return Ok(());
    }
    let route: Option<i64> = connection
        .query_row(
            "SELECT route FROM web_search_attempts WHERE call_id=?1 AND child=0 AND ordinal=0",
            [binding.call_id.as_bytes()],
            |r| r.get(0),
        )
        .optional()?;
    // Only pre-migration bindings may lack durable per-invocation admission.
    if route.is_some_and(|route| (route != 0) != exa)
        || (route.is_none() && sequence_to_sql(binding.sequence)? >= epoch)
    {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_digest_preserves_legacy_encoding_and_separates_wide_ordinals() {
        let binding = WebBinding {
            run_id: crate::persistence::RunId::from_bytes([1; 16]),
            call_id: crate::persistence::ToolCallId::from_bytes([2; 16]),
            operation_id: [3; 16],
            generation: 1,
            absence_generation: None,
            exa_contract_revision: 0,
            policy_sequence: 4,
            sequence: 5,
            query_digest: None,
            children: 3,
        };
        for ordinal in [1, 24, 25, 65535, 65536, i64::MAX as u64] {
            let invocation = WebInvocation {
                child: 1,
                ordinal,
                query_digest: [6; 32],
                route: WebRoute::OpenAi,
            };
            let mut bytes = if ordinal <= 65535 {
                b"morons.dev/web-search-admission/v1\0".to_vec()
            } else {
                b"morons.dev/web-search-admission/v2\0".to_vec()
            };
            bytes.extend(digest(&binding, &[]));
            bytes.extend(1_u16.to_be_bytes());
            if ordinal <= 65535 {
                bytes.extend((ordinal as u16).to_be_bytes());
            } else {
                bytes.extend(ordinal.to_be_bytes());
            }
            bytes.extend([6; 32]);
            bytes.extend(0_i64.to_be_bytes());
            bytes.extend(7_u64.to_be_bytes());
            bytes.extend(8_u64.to_be_bytes());
            let expected: [u8; 32] = Sha256::digest(bytes).into();
            assert_eq!(
                admission_digest(&binding, &invocation, 7, 8),
                expected,
                "ordinal {ordinal}"
            );
        }
    }
}
