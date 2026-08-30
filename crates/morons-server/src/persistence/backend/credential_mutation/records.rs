use rusqlite::{OptionalExtension, Transaction, params};

use super::super::records::{sequence_to_sql, time_to_sql};
use crate::persistence::{
    MutationRequestId, OpenCodeCredentialStatus, PersistenceError, PersistenceResourceLimit,
};

pub(super) const CREDENTIAL_REQUEST_PREPARED: i64 = 0;
pub(super) const CREDENTIAL_REQUEST_DISPATCHED: i64 = 1;
pub(super) const CREDENTIAL_REQUEST_COMPLETED: i64 = 2;
pub(super) const CREDENTIAL_REQUEST_NOT_APPLIED: i64 = 3;

pub(super) const CREDENTIAL_FACT_DISPATCHED: i64 = 1;
pub(super) const CREDENTIAL_FACT_COMPLETED: i64 = 2;
pub(super) const CREDENTIAL_FACT_NOT_APPLIED: i64 = 3;

pub(super) const CREDENTIAL_AUDIT_ACCEPTED: i64 = 1;
pub(super) const CREDENTIAL_AUDIT_DISPATCHED: i64 = 2;
pub(super) const CREDENTIAL_AUDIT_COMPLETED: i64 = 3;
pub(super) const CREDENTIAL_AUDIT_NOT_APPLIED: i64 = 4;

#[derive(Debug)]
pub(super) struct CredentialMutationRequest {
    pub(super) request_id: MutationRequestId,
    pub(super) operation_kind: i64,
    pub(super) expected_generation: u64,
    pub(super) accepted_sequence: u64,
    pub(super) accepted_at_milliseconds: u64,
    pub(super) state: i64,
    pub(super) result: Option<OpenCodeCredentialStatus>,
}

fn load_credential_request(
    connection: &rusqlite::Connection,
    request_id: MutationRequestId,
) -> Result<Option<CredentialMutationRequest>, PersistenceError> {
    connection
        .query_row(
            "SELECT
                request_id,
                operation_kind,
                expected_generation,
                accepted_sequence,
                accepted_at_milliseconds,
                state,
                result_generation,
                result_configured
             FROM credential_mutation_requests
             WHERE request_id = ?1",
            [&request_id.as_bytes()[..]],
            credential_request_from_row,
        )
        .optional()
        .map_err(PersistenceError::from)
}

pub(super) fn load_required_credential_request(
    connection: &rusqlite::Connection,
    request_id: MutationRequestId,
) -> Result<CredentialMutationRequest, PersistenceError> {
    load_credential_request(connection, request_id)?.ok_or(PersistenceError::InvalidState {
        reason: "a credential mutation is missing its request record",
    })
}

pub(super) fn load_incomplete_credential_requests(
    connection: &rusqlite::Connection,
) -> Result<Vec<CredentialMutationRequest>, PersistenceError> {
    let mut statement = connection.prepare(
        "SELECT
            request_id,
            operation_kind,
            expected_generation,
            accepted_sequence,
            accepted_at_milliseconds,
            state,
            result_generation,
            result_configured
         FROM credential_mutation_requests
         WHERE state IN (?1, ?2)
         ORDER BY accepted_sequence",
    )?;
    statement
        .query_map(
            [CREDENTIAL_REQUEST_PREPARED, CREDENTIAL_REQUEST_DISPATCHED],
            credential_request_from_row,
        )?
        .collect::<Result<Vec<_>, _>>()
        .map_err(PersistenceError::from)
}

fn credential_request_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<CredentialMutationRequest> {
    let result_generation = row
        .get::<_, Option<i64>>(6)?
        .map(|value| {
            u64::try_from(value).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    6,
                    rusqlite::types::Type::Integer,
                    Box::new(error),
                )
            })
        })
        .transpose()?;
    let result_configured = row.get::<_, Option<i64>>(7)?;
    let result = match (result_generation, result_configured) {
        (Some(generation), Some(configured)) => Some(OpenCodeCredentialStatus {
            configured: configured != 0,
            generation,
        }),
        (None, None) => None,
        _ => {
            return Err(rusqlite::Error::InvalidColumnType(
                6,
                "result_generation".to_owned(),
                rusqlite::types::Type::Null,
            ));
        }
    };
    Ok(CredentialMutationRequest {
        request_id: MutationRequestId::from_bytes(row.get(0)?),
        operation_kind: row.get(1)?,
        expected_generation: nonnegative_integer_from_row(row, 2)?,
        accepted_sequence: nonnegative_integer_from_row(row, 3)?,
        accepted_at_milliseconds: nonnegative_integer_from_row(row, 4)?,
        state: row.get(5)?,
        result,
    })
}

pub(super) fn validate_request_identity(
    request: &CredentialMutationRequest,
    operation_kind: i64,
    expected_generation: u64,
) -> Result<(), PersistenceError> {
    if request.operation_kind != operation_kind
        || request.expected_generation != expected_generation
    {
        return Err(PersistenceError::RequestConflict);
    }
    Ok(())
}

pub(super) fn completed_request_result(
    request: &CredentialMutationRequest,
) -> Result<OpenCodeCredentialStatus, PersistenceError> {
    match (request.state, request.result) {
        (CREDENTIAL_REQUEST_COMPLETED, Some(result)) => Ok(result),
        (CREDENTIAL_REQUEST_NOT_APPLIED, None) => {
            Err(PersistenceError::CredentialMutationNotApplied)
        }
        _ => Err(PersistenceError::InvalidState {
            reason: "a credential mutation did not reach a recoverable outcome",
        }),
    }
}

pub(super) fn validate_current_request(
    transaction: &Transaction<'_>,
    expected: &CredentialMutationRequest,
    expected_state: i64,
) -> Result<(), PersistenceError> {
    let current = load_credential_request(transaction, expected.request_id)?.ok_or(
        PersistenceError::InvalidState {
            reason: "a credential mutation disappeared during execution",
        },
    )?;
    if current.operation_kind != expected.operation_kind
        || current.expected_generation != expected.expected_generation
        || current.accepted_sequence != expected.accepted_sequence
        || current.accepted_at_milliseconds != expected.accepted_at_milliseconds
        || current.state != expected_state
    {
        return Err(PersistenceError::InvalidState {
            reason: "a credential mutation changed identity during execution",
        });
    }
    Ok(())
}

pub(super) fn insert_credential_operation_fact(
    transaction: &Transaction<'_>,
    fact_id: &[u8; 16],
    fact_sequence: u64,
    request_id: MutationRequestId,
    operation_kind: i64,
    credential_generation: u64,
    created_at_milliseconds: u64,
) -> Result<(), PersistenceError> {
    transaction.execute(
        "INSERT INTO credential_operation_facts (
            fact_id,
            fact_sequence,
            request_id,
            operation_kind,
            credential_generation,
            created_at_milliseconds
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            &fact_id[..],
            sequence_to_sql(fact_sequence)?,
            &request_id.as_bytes()[..],
            operation_kind,
            sequence_to_sql(credential_generation)?,
            time_to_sql(created_at_milliseconds)?,
        ],
    )?;
    Ok(())
}

pub(super) fn insert_credential_audit_fact(
    transaction: &Transaction<'_>,
    audit_id: &[u8; 16],
    audit_sequence: u64,
    request_id: MutationRequestId,
    audit_kind: i64,
    created_at_milliseconds: u64,
) -> Result<(), PersistenceError> {
    transaction.execute(
        "INSERT INTO credential_audit_facts (
            audit_id,
            audit_sequence,
            request_id,
            actor_kind,
            audit_kind,
            created_at_milliseconds
         ) VALUES (?1, ?2, ?3, 1, ?4, ?5)",
        params![
            &audit_id[..],
            sequence_to_sql(audit_sequence)?,
            &request_id.as_bytes()[..],
            audit_kind,
            time_to_sql(created_at_milliseconds)?,
        ],
    )?;
    Ok(())
}

pub(super) fn update_credential_request_outcome(
    transaction: &Transaction<'_>,
    request_id: MutationRequestId,
    expected_state: i64,
    next_state: i64,
    result: Option<OpenCodeCredentialStatus>,
) -> Result<(), PersistenceError> {
    let changed = transaction.execute(
        "UPDATE credential_mutation_requests
         SET state = ?2,
             result_generation = ?3,
             result_configured = ?4
         WHERE request_id = ?1 AND state = ?5",
        params![
            &request_id.as_bytes()[..],
            next_state,
            result
                .map(|result| sequence_to_sql(result.generation))
                .transpose()?,
            result.map(|result| if result.configured { 1_i64 } else { 0_i64 }),
            expected_state,
        ],
    )?;
    if changed != 1 {
        return Err(PersistenceError::InvalidState {
            reason: "a credential mutation changed before its outcome",
        });
    }
    Ok(())
}

pub(super) fn validate_credential_request_records(
    connection: &rusqlite::Connection,
) -> Result<(), PersistenceError> {
    let invalid: bool = connection.query_row(
        "SELECT EXISTS (
            SELECT 1
            FROM credential_mutation_requests AS request
            LEFT JOIN mutation_requests AS mutation ON mutation.request_id = request.request_id
            WHERE mutation.request_id IS NULL
               OR mutation.operation_kind IS NOT request.operation_kind
               OR mutation.accepted_sequence IS NOT request.accepted_sequence
               OR mutation.accepted_at_milliseconds IS NOT request.accepted_at_milliseconds
               OR (request.state = 2 AND (
                    request.result_generation != request.expected_generation + 1
                    OR request.result_configured != CASE WHEN request.operation_kind = 2 THEN 1 ELSE 0 END
               ))
               OR EXISTS (
                    SELECT 1 FROM credential_operation_facts AS fact
                    WHERE fact.request_id = request.request_id
                      AND fact.credential_generation != request.expected_generation + 1
               )
               OR (SELECT COUNT(*) FROM credential_audit_facts AS audit
                   WHERE audit.request_id = request.request_id AND audit.audit_kind = 1) != 1
               OR (SELECT COUNT(*) FROM credential_operation_facts AS fact
                   WHERE fact.request_id = request.request_id AND fact.operation_kind = 1)
                  != (SELECT COUNT(*) FROM credential_audit_facts AS audit
                      WHERE audit.request_id = request.request_id AND audit.audit_kind = 2)
               OR (request.state IN (1, 2) AND
                   (SELECT COUNT(*) FROM credential_operation_facts AS fact
                    WHERE fact.request_id = request.request_id AND fact.operation_kind = 1) != 1)
               OR (request.state = 0 AND
                   (SELECT COUNT(*) FROM credential_operation_facts AS fact
                    WHERE fact.request_id = request.request_id AND fact.operation_kind = 1) != 0)
               OR (SELECT COUNT(*) FROM credential_operation_facts AS fact
                   WHERE fact.request_id = request.request_id AND fact.operation_kind = 2)
                  != CASE WHEN request.state = 2 THEN 1 ELSE 0 END
               OR (SELECT COUNT(*) FROM credential_audit_facts AS audit
                   WHERE audit.request_id = request.request_id AND audit.audit_kind = 3)
                  != CASE WHEN request.state = 2 THEN 1 ELSE 0 END
               OR (SELECT COUNT(*) FROM credential_operation_facts AS fact
                   WHERE fact.request_id = request.request_id AND fact.operation_kind = 3)
                  != CASE WHEN request.state = 3 THEN 1 ELSE 0 END
               OR (SELECT COUNT(*) FROM credential_audit_facts AS audit
                   WHERE audit.request_id = request.request_id AND audit.audit_kind = 4)
                  != CASE WHEN request.state = 3 THEN 1 ELSE 0 END
            UNION ALL
            SELECT 1
            FROM mutation_requests AS mutation
            LEFT JOIN credential_mutation_requests AS request
                ON request.request_id = mutation.request_id
            WHERE mutation.operation_kind IN (2, 3) AND request.request_id IS NULL
        )",
        [],
        |row| row.get(0),
    )?;
    if invalid {
        return Err(PersistenceError::InvalidState {
            reason: "credential mutation facts conflict with request state",
        });
    }
    Ok(())
}

pub(super) fn next_credential_generation(current: u64) -> Result<u64, PersistenceError> {
    current
        .checked_add(1)
        .filter(|generation| *generation <= i64::MAX as u64)
        .ok_or(PersistenceError::ResourceLimit {
            resource: PersistenceResourceLimit::CredentialGeneration,
        })
}

fn nonnegative_integer_from_row(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value = row.get::<_, i64>(index)?;
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}
