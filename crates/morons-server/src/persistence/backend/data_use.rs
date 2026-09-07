use rusqlite::{Connection, OptionalExtension as _, TransactionBehavior, params};
use sha2::{Digest as _, Sha256};

use super::{
    Backend,
    records::{
        current_time_milliseconds, load_mutation_operation, next_sequence, sequence_to_sql,
        time_to_sql,
    },
};
use crate::{
    persistence::{
        DataUsePolicy, MutationRequestId, PersistenceError, PersistenceResourceLimit,
        RunOpenCodeService,
    },
    provider::{DataUseRestrictions, OpenCodeService, find_open_code_model},
};

const OPERATION: i64 = 17;
const MAX_POLICIES: i64 = 10_000;

impl Backend {
    pub(crate) fn data_use_policy(&self) -> Result<DataUsePolicy, PersistenceError> {
        self.ensure_context_integrity()?;
        current(&self.connection)
    }

    pub(crate) fn admit_model_data_use(
        &self,
        service: RunOpenCodeService,
        model: &str,
    ) -> Result<DataUsePolicy, PersistenceError> {
        self.ensure_context_integrity()?;
        admit(&self.connection, service, model)
    }

    pub(crate) fn set_data_use_policy(
        &mut self,
        id: MutationRequestId,
        expected_sequence: u64,
        restrictions: DataUseRestrictions,
    ) -> Result<DataUsePolicy, PersistenceError> {
        self.validate_data_use_policy()?;
        let fingerprint = fingerprint(expected_sequence, restrictions);
        let existing = self.connection.query_row(
            "SELECT operation_fingerprint, accepted_sequence, block_training_use, require_zero_retention
             FROM data_use_policies WHERE request_id = ?1",
            [&id.as_bytes()[..]],
            |row| Ok((row.get::<_, [u8; 32]>(0)?, policy_from_columns(row, 1)?)),
        ).optional()?;
        match (load_mutation_operation(&self.connection, id)?, existing) {
            (Some(OPERATION), Some((stored, policy))) if stored == fingerprint => {
                return Ok(policy);
            }
            (None, None) => {}
            _ => return Err(PersistenceError::RequestConflict),
        }
        self.ensure_context_integrity()?;
        let count: i64 =
            self.connection
                .query_row("SELECT COUNT(*) FROM data_use_policies", [], |row| {
                    row.get(0)
                })?;
        if count >= MAX_POLICIES {
            return Err(PersistenceError::ResourceLimit {
                resource: PersistenceResourceLimit::DataUsePolicies,
            });
        }
        let now = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if current(&transaction)?.sequence != expected_sequence {
            return Err(PersistenceError::DataUsePolicyChanged);
        }
        let sequence = next_sequence(&transaction)?;
        transaction.execute(
            "INSERT INTO mutation_requests (request_id, operation_kind, accepted_sequence, accepted_at_milliseconds)
             VALUES (?1, ?2, ?3, ?4)",
            params![&id.as_bytes()[..], OPERATION, sequence_to_sql(sequence)?, time_to_sql(now)?],
        )?;
        transaction.execute(
            "INSERT INTO data_use_policies (request_id, operation_fingerprint, expected_sequence, accepted_sequence,
                block_training_use, require_zero_retention, accepted_at_milliseconds)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![&id.as_bytes()[..], &fingerprint[..], sequence_to_sql(expected_sequence)?,
                sequence_to_sql(sequence)?, restrictions.block_training_use,
                restrictions.require_zero_retention, time_to_sql(now)?],
        )?;
        transaction.commit()?;
        Ok(DataUsePolicy {
            sequence,
            restrictions,
        })
    }

    pub(super) fn validate_data_use_policy(&self) -> Result<(), PersistenceError> {
        let count: i64 =
            self.connection
                .query_row("SELECT COUNT(*) FROM data_use_policies", [], |row| {
                    row.get(0)
                })?;
        if count > MAX_POLICIES {
            return Err(invalid());
        }
        let inconsistent: bool = self.connection.query_row(
            "SELECT EXISTS (
                SELECT 1 FROM mutation_requests AS mutation LEFT JOIN data_use_policies AS policy USING (request_id)
                WHERE mutation.operation_kind = 17 AND policy.request_id IS NULL
                UNION ALL
                SELECT 1 FROM data_use_policies AS policy LEFT JOIN mutation_requests AS mutation USING (request_id)
                WHERE mutation.request_id IS NULL OR mutation.operation_kind != 17
                    OR mutation.accepted_sequence != policy.accepted_sequence
                    OR mutation.accepted_at_milliseconds != policy.accepted_at_milliseconds
                    OR policy.accepted_sequence >= (SELECT next_value FROM logical_sequences WHERE singleton = 1))",
            [], |row| row.get(0),
        )?;
        if inconsistent {
            return Err(invalid());
        }
        let mut statement = self.connection.prepare(
            "SELECT operation_fingerprint, expected_sequence, accepted_sequence, block_training_use, require_zero_retention
             FROM data_use_policies ORDER BY accepted_sequence",
        )?;
        let mut rows = statement.query([])?;
        let mut previous = 0;
        while let Some(row) = rows.next()? {
            let stored = row.get::<_, [u8; 32]>(0)?;
            let expected = super::records::nonnegative_integer_from_row(row, 1)?;
            let policy = policy_from_columns(row, 2)?;
            if expected != previous
                || policy.sequence <= expected
                || stored != fingerprint(expected, policy.restrictions)
            {
                return Err(invalid());
            }
            previous = policy.sequence;
        }
        Ok(())
    }
}

pub(super) fn current(connection: &Connection) -> Result<DataUsePolicy, PersistenceError> {
    connection
        .query_row(
            "SELECT accepted_sequence, block_training_use, require_zero_retention
         FROM data_use_policies ORDER BY accepted_sequence DESC LIMIT 1",
            [],
            |row| policy_from_columns(row, 0),
        )
        .optional()
        .map(|policy| policy.unwrap_or_default())
        .map_err(Into::into)
}

pub(super) fn admit(
    connection: &Connection,
    service: RunOpenCodeService,
    model: &str,
) -> Result<DataUsePolicy, PersistenceError> {
    let policy = current(connection)?;
    if policy.restrictions == DataUseRestrictions::default() {
        return Ok(policy);
    }
    let service = match service {
        RunOpenCodeService::Zen => OpenCodeService::Zen,
        RunOpenCodeService::Go => OpenCodeService::Go,
    };
    if !find_open_code_model(service, model)
        .is_some_and(|model| policy.restrictions.permits(model.data_use))
    {
        return Err(PersistenceError::DataUseRestricted);
    }
    Ok(policy)
}

fn policy_from_columns(row: &rusqlite::Row<'_>, start: usize) -> rusqlite::Result<DataUsePolicy> {
    let training = row.get::<_, i64>(start + 1)?;
    let retention = row.get::<_, i64>(start + 2)?;
    if !matches!(training, 0 | 1) || !matches!(retention, 0 | 1) {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(DataUsePolicy {
        sequence: super::records::nonnegative_integer_from_row(row, start)?,
        restrictions: DataUseRestrictions {
            block_training_use: training == 1,
            require_zero_retention: retention == 1,
        },
    })
}

fn fingerprint(expected: u64, restrictions: DataUseRestrictions) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"morons.dev/data-use-policy/v1\0");
    hash.update(expected.to_be_bytes());
    hash.update([
        u8::from(restrictions.block_training_use),
        u8::from(restrictions.require_zero_retention),
    ]);
    hash.finalize().into()
}
#[cfg(test)]
#[test]
fn data_use_policy_ledger_is_bounded_without_partially_committing_a_mutation() {
    let root = crate::persistence::credential_tests::TestRoot::new("policy-capacity");
    let mut backend = Backend::open(root.path()).unwrap();
    let restrictions = DataUseRestrictions::default();
    let transaction = backend.connection.transaction().unwrap();
    for sequence in 1..=MAX_POLICIES {
        let id = u128::try_from(sequence).unwrap().to_be_bytes();
        let fingerprint = fingerprint(u64::try_from(sequence - 1).unwrap(), restrictions);
        transaction.execute("INSERT INTO mutation_requests (request_id, operation_kind, accepted_sequence, accepted_at_milliseconds) VALUES (?1,17,?2,0)", params![&id[..],sequence]).unwrap();
        transaction.execute("INSERT INTO data_use_policies (request_id, operation_fingerprint, expected_sequence, accepted_sequence, block_training_use, require_zero_retention, accepted_at_milliseconds) VALUES (?1,?2,?3,?4,0,0,0)",params![&id[..],&fingerprint[..],sequence-1,sequence]).unwrap();
    }
    transaction
        .execute(
            "UPDATE logical_sequences SET next_value = ?1",
            [MAX_POLICIES + 1],
        )
        .unwrap();
    transaction.commit().unwrap();
    assert!(matches!(
        backend.set_data_use_policy(
            MutationRequestId::from_bytes([0xff; 16]),
            u64::try_from(MAX_POLICIES).unwrap(),
            restrictions
        ),
        Err(PersistenceError::ResourceLimit {
            resource: PersistenceResourceLimit::DataUsePolicies
        })
    ));
    assert_eq!(
        backend
            .connection
            .query_row("SELECT COUNT(*) FROM mutation_requests", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        MAX_POLICIES
    );
}

fn invalid() -> PersistenceError {
    PersistenceError::InvalidState {
        reason: "data-use policy evidence is invalid or inconsistent",
    }
}
