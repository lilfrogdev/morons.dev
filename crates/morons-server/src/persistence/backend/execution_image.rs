use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use super::{
    Backend,
    records::{
        current_time_milliseconds, load_mutation_operation, next_sequence, random_identifier,
        sequence_to_sql, time_to_sql,
    },
};
use crate::persistence::{
    ExecutionImageOutcome, ExecutionImagePlan, ExecutionImageState, ExecutionImageSummary,
    ExecutionTargetArch, ExecutionTargetOs, MutationRequestId, PersistenceError,
    PersistenceResourceLimit, types::REQUEST_FINGERPRINT_BYTES,
};

pub(super) const MUTATION_OPERATION_PROVISION_EXECUTION_IMAGE: i64 = 9;
pub(super) const IMAGE_STATE_PREPARED: u8 = 0;
pub(super) const IMAGE_STATE_DISPATCHED: u8 = 1;
pub(super) const IMAGE_STATE_READY: u8 = 2;
pub(super) const IMAGE_STATE_NOT_APPLIED: u8 = 3;
pub(super) const IMAGE_STATE_BLOCKED: u8 = 4;
const FACT_PREPARED: i64 = 1;
const FACT_DISPATCHED: i64 = 2;
const FACT_COMPLETED: i64 = 3;
const FACT_NOT_APPLIED: i64 = 4;
const FACT_BLOCKED: i64 = 5;
const FORMAT_VERSION: u16 = 1;
const LIMITS_VERSION: u16 = 1;
const MAX_PROVISION_REQUESTS: i64 = 1_000;

impl Backend {
    pub(super) fn recover_execution_images(&mut self) -> Result<(), PersistenceError> {
        let plans = self.execution_images_for_recovery()?;
        let paths = self.paths.clone();
        for plan in plans {
            if plan.state == IMAGE_STATE_PREPARED {
                self.mark_execution_image_not_applied(plan)?;
                continue;
            }
            match paths.recover_execution_image(plan)? {
                crate::persistence::execution_image::ExecutionImageRecovery::Complete(outcome) => {
                    self.complete_execution_image(plan, outcome)?;
                }
                crate::persistence::execution_image::ExecutionImageRecovery::NotApplied => {
                    self.mark_execution_image_not_applied(plan)?;
                }
                crate::persistence::execution_image::ExecutionImageRecovery::Blocked => {
                    self.block_execution_image(plan)?;
                }
            }
        }
        let active = self.current_execution_image()?;
        if let Some((plan, outcome)) = active {
            paths.validate_execution_image(plan, outcome)?;
            paths.cleanup_inactive_execution_images(Some(plan.generation_id))?;
        } else {
            paths.cleanup_inactive_execution_images(None)?;
        }
        Ok(())
    }

    pub(crate) fn prepare_execution_image(
        &mut self,
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        toolchain_source_digest: [u8; REQUEST_FINGERPRINT_BYTES],
        cargo_source_digest: [u8; REQUEST_FINGERPRINT_BYTES],
    ) -> Result<ExecutionImagePlan, PersistenceError> {
        if let Some(existing) = load_plan(&self.connection, request_id)? {
            validate_retry(
                &self.connection,
                existing,
                fingerprint,
                toolchain_source_digest,
                cargo_source_digest,
            )?;
            return Ok(existing);
        }
        if load_mutation_operation(&self.connection, request_id)?.is_some() {
            return Err(PersistenceError::RequestConflict);
        }
        let generation_id = random_identifier()?;
        let operation_id = random_identifier()?;
        let target_os = current_target_os();
        let target_arch = current_target_arch();
        let fact_id = random_identifier()?;
        let audit_id = random_identifier()?;
        let accepted_at = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let request_count: i64 =
            transaction.query_row("SELECT COUNT(*) FROM execution_image_requests", [], |row| {
                row.get(0)
            })?;
        if request_count >= MAX_PROVISION_REQUESTS {
            return Err(PersistenceError::ResourceLimit {
                resource: PersistenceResourceLimit::ExecutionImage,
            });
        }
        let incomplete: bool = transaction.query_row(
            "SELECT EXISTS (SELECT 1 FROM execution_image_requests WHERE state IN (0, 1))",
            [],
            |row| row.get(0),
        )?;
        if incomplete {
            return Err(PersistenceError::ExecutionImageBlocked);
        }
        let accepted_sequence = next_sequence(&transaction)?;
        let fact_sequence = next_sequence(&transaction)?;
        let audit_sequence = next_sequence(&transaction)?;
        transaction.execute(
            "INSERT INTO mutation_requests (
                request_id, operation_kind, accepted_sequence, accepted_at_milliseconds
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                &request_id.as_bytes()[..],
                MUTATION_OPERATION_PROVISION_EXECUTION_IMAGE,
                sequence_to_sql(accepted_sequence)?,
                time_to_sql(accepted_at)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO execution_image_requests (
                request_id, operation_fingerprint, toolchain_source_digest,
                cargo_source_digest, generation_id, operation_id, target_os,
                target_arch, format_version, limits_version, accepted_sequence,
                accepted_at_milliseconds, state
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                &request_id.as_bytes()[..],
                &fingerprint[..],
                &toolchain_source_digest[..],
                &cargo_source_digest[..],
                &generation_id[..],
                &operation_id[..],
                target_os.to_record(),
                target_arch.to_record(),
                i64::from(FORMAT_VERSION),
                i64::from(LIMITS_VERSION),
                sequence_to_sql(accepted_sequence)?,
                time_to_sql(accepted_at)?,
                i64::from(IMAGE_STATE_PREPARED),
            ],
        )?;
        insert_fact(
            &transaction,
            &fact_id,
            fact_sequence,
            request_id,
            &generation_id,
            &operation_id,
            FACT_PREPARED,
            None,
            accepted_at,
        )?;
        insert_audit(
            &transaction,
            &audit_id,
            audit_sequence,
            request_id,
            &generation_id,
            &operation_id,
            FACT_PREPARED,
            accepted_at,
        )?;
        transaction.commit()?;
        Ok(ExecutionImagePlan {
            request_id,
            generation_id,
            operation_id,
            target_os,
            target_arch,
            state: IMAGE_STATE_PREPARED,
        })
    }

    pub(crate) fn dispatch_execution_image(
        &mut self,
        expected: ExecutionImagePlan,
    ) -> Result<ExecutionImagePlan, PersistenceError> {
        let current = load_plan(&self.connection, expected.request_id)?.ok_or(
            PersistenceError::InvalidState {
                reason: "an execution image request disappeared before dispatch",
            },
        )?;
        validate_plan(current, expected)?;
        if current.state != IMAGE_STATE_PREPARED {
            return Ok(current);
        }
        let fact_id = random_identifier()?;
        let audit_id = random_identifier()?;
        let created_at = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let fact_sequence = next_sequence(&transaction)?;
        let audit_sequence = next_sequence(&transaction)?;
        insert_fact(
            &transaction,
            &fact_id,
            fact_sequence,
            current.request_id,
            &current.generation_id,
            &current.operation_id,
            FACT_DISPATCHED,
            None,
            created_at,
        )?;
        insert_audit(
            &transaction,
            &audit_id,
            audit_sequence,
            current.request_id,
            &current.generation_id,
            &current.operation_id,
            FACT_DISPATCHED,
            created_at,
        )?;
        let updated = transaction.execute(
            "UPDATE execution_image_requests SET state = ?2
             WHERE request_id = ?1 AND state = ?3",
            params![
                &current.request_id.as_bytes()[..],
                i64::from(IMAGE_STATE_DISPATCHED),
                i64::from(IMAGE_STATE_PREPARED),
            ],
        )?;
        if updated != 1 {
            return Err(PersistenceError::InvalidState {
                reason: "an execution image request did not reach dispatched state",
            });
        }
        transaction.commit()?;
        Ok(ExecutionImagePlan {
            state: IMAGE_STATE_DISPATCHED,
            ..current
        })
    }

    pub(crate) fn complete_execution_image(
        &mut self,
        expected: ExecutionImagePlan,
        outcome: ExecutionImageOutcome,
    ) -> Result<ExecutionImageSummary, PersistenceError> {
        self.finalize_execution_image(expected, IMAGE_STATE_READY, Some(outcome))
    }

    pub(crate) fn mark_execution_image_not_applied(
        &mut self,
        expected: ExecutionImagePlan,
    ) -> Result<ExecutionImageSummary, PersistenceError> {
        self.finalize_execution_image(expected, IMAGE_STATE_NOT_APPLIED, None)
    }

    pub(crate) fn block_execution_image(
        &mut self,
        expected: ExecutionImagePlan,
    ) -> Result<ExecutionImageSummary, PersistenceError> {
        self.finalize_execution_image(expected, IMAGE_STATE_BLOCKED, None)
    }

    pub(crate) fn execution_image_summary(
        &self,
    ) -> Result<ExecutionImageSummary, PersistenceError> {
        if let Some((plan, outcome)) = self.current_execution_image()? {
            return Ok(summary(ExecutionImageState::Ready, plan, Some(outcome)));
        }
        let latest = self
            .connection
            .query_row(
                "SELECT request_id, generation_id, operation_id, target_os, target_arch, state
                 FROM execution_image_requests ORDER BY accepted_sequence DESC LIMIT 1",
                [],
                plan_from_row,
            )
            .optional()?;
        Ok(match latest {
            Some(plan) if matches!(plan.state, IMAGE_STATE_PREPARED | IMAGE_STATE_DISPATCHED) => {
                summary(ExecutionImageState::Provisioning, plan, None)
            }
            Some(plan) if plan.state == IMAGE_STATE_BLOCKED => {
                summary(ExecutionImageState::Blocked, plan, None)
            }
            Some(plan) => summary(ExecutionImageState::Unconfigured, plan, None),
            None => summary(
                ExecutionImageState::Unconfigured,
                ExecutionImagePlan {
                    request_id: MutationRequestId::from_bytes([0; 16]),
                    generation_id: [0; 16],
                    operation_id: [0; 16],
                    target_os: current_target_os(),
                    target_arch: current_target_arch(),
                    state: IMAGE_STATE_NOT_APPLIED,
                },
                None,
            ),
        })
    }

    fn finalize_execution_image(
        &mut self,
        expected: ExecutionImagePlan,
        target_state: u8,
        outcome: Option<ExecutionImageOutcome>,
    ) -> Result<ExecutionImageSummary, PersistenceError> {
        let current = load_plan(&self.connection, expected.request_id)?.ok_or(
            PersistenceError::InvalidState {
                reason: "an execution image request disappeared before completion",
            },
        )?;
        validate_plan(current, expected)?;
        if current.state == target_state {
            return self.execution_image_summary();
        }
        if current.state != IMAGE_STATE_DISPATCHED
            && !(current.state == IMAGE_STATE_PREPARED && target_state == IMAGE_STATE_NOT_APPLIED)
        {
            return Err(PersistenceError::InvalidState {
                reason: "an execution image request has an invalid terminal transition",
            });
        }
        if (target_state == IMAGE_STATE_READY) != outcome.is_some()
            || !matches!(
                target_state,
                IMAGE_STATE_READY | IMAGE_STATE_NOT_APPLIED | IMAGE_STATE_BLOCKED
            )
        {
            return Err(PersistenceError::InvalidState {
                reason: "an execution image terminal outcome is invalid",
            });
        }
        let fact_kind = match target_state {
            IMAGE_STATE_READY => FACT_COMPLETED,
            IMAGE_STATE_NOT_APPLIED => FACT_NOT_APPLIED,
            IMAGE_STATE_BLOCKED => FACT_BLOCKED,
            _ => unreachable!(),
        };
        let fact_id = random_identifier()?;
        let audit_id = random_identifier()?;
        let created_at = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let fact_sequence = next_sequence(&transaction)?;
        let audit_sequence = next_sequence(&transaction)?;
        insert_fact(
            &transaction,
            &fact_id,
            fact_sequence,
            current.request_id,
            &current.generation_id,
            &current.operation_id,
            fact_kind,
            outcome,
            created_at,
        )?;
        insert_audit(
            &transaction,
            &audit_id,
            audit_sequence,
            current.request_id,
            &current.generation_id,
            &current.operation_id,
            fact_kind,
            created_at,
        )?;
        let values = outcome.map(|value| {
            (
                value.file_count,
                value.directory_count,
                value.logical_bytes,
                value.manifest_digest,
            )
        });
        let updated = transaction.execute(
            "UPDATE execution_image_requests
             SET state = ?2, file_count = ?3, directory_count = ?4,
                 logical_bytes = ?5, manifest_digest = ?6
             WHERE request_id = ?1 AND state = ?7",
            params![
                &current.request_id.as_bytes()[..],
                i64::from(target_state),
                values.map(|value| sequence_to_sql(value.0)).transpose()?,
                values.map(|value| sequence_to_sql(value.1)).transpose()?,
                values.map(|value| sequence_to_sql(value.2)).transpose()?,
                values.map(|value| value.3.to_vec()),
                i64::from(current.state),
            ],
        )?;
        if updated != 1 {
            return Err(PersistenceError::InvalidState {
                reason: "an execution image request did not reach terminal state",
            });
        }
        if target_state == IMAGE_STATE_READY {
            transaction.execute(
                "INSERT INTO current_execution_image (
                    singleton, request_id, generation_id, updated_sequence
                 ) VALUES (1, ?1, ?2, ?3)
                 ON CONFLICT(singleton) DO UPDATE SET
                    request_id = excluded.request_id,
                    generation_id = excluded.generation_id,
                    updated_sequence = excluded.updated_sequence",
                params![
                    &current.request_id.as_bytes()[..],
                    &current.generation_id[..],
                    sequence_to_sql(fact_sequence)?,
                ],
            )?;
        }
        transaction.commit()?;
        self.execution_image_summary()
    }

    fn current_execution_image(
        &self,
    ) -> Result<Option<(ExecutionImagePlan, ExecutionImageOutcome)>, PersistenceError> {
        self.connection
            .query_row(
                "SELECT request.request_id, request.generation_id, request.operation_id,
                        request.target_os, request.target_arch, request.state,
                        request.file_count, request.directory_count,
                        request.logical_bytes, request.manifest_digest
                 FROM current_execution_image AS current
                 JOIN execution_image_requests AS request
                   ON request.request_id = current.request_id
                  AND request.generation_id = current.generation_id
                 WHERE current.singleton = 1",
                [],
                |row| {
                    let plan = plan_from_row(row)?;
                    let file_count = nonnegative(row.get(6)?, 6)?;
                    let directory_count = nonnegative(row.get(7)?, 7)?;
                    let logical_bytes = nonnegative(row.get(8)?, 8)?;
                    Ok((
                        plan,
                        ExecutionImageOutcome {
                            file_count,
                            directory_count,
                            logical_bytes,
                            manifest_digest: row.get(9)?,
                        },
                    ))
                },
            )
            .optional()
            .map_err(PersistenceError::from)
    }

    fn execution_images_for_recovery(&self) -> Result<Vec<ExecutionImagePlan>, PersistenceError> {
        let mut statement = self.connection.prepare(
            "SELECT request_id, generation_id, operation_id, target_os, target_arch, state
             FROM execution_image_requests WHERE state IN (0, 1)
             ORDER BY accepted_sequence",
        )?;
        statement
            .query_map([], plan_from_row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(PersistenceError::from)
    }
}

fn load_plan(
    connection: &rusqlite::Connection,
    request_id: MutationRequestId,
) -> Result<Option<ExecutionImagePlan>, PersistenceError> {
    connection
        .query_row(
            "SELECT request_id, generation_id, operation_id, target_os, target_arch, state
             FROM execution_image_requests WHERE request_id = ?1",
            [&request_id.as_bytes()[..]],
            plan_from_row,
        )
        .optional()
        .map_err(PersistenceError::from)
}

fn plan_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExecutionImagePlan> {
    let target_os = row.get::<_, i64>(3)?;
    let target_arch = row.get::<_, i64>(4)?;
    let state = row.get::<_, i64>(5)?;
    Ok(ExecutionImagePlan {
        request_id: MutationRequestId::from_bytes(row.get(0)?),
        generation_id: row.get(1)?,
        operation_id: row.get(2)?,
        target_os: ExecutionTargetOs::from_record(target_os)
            .ok_or(rusqlite::Error::IntegralValueOutOfRange(3, target_os))?,
        target_arch: ExecutionTargetArch::from_record(target_arch)
            .ok_or(rusqlite::Error::IntegralValueOutOfRange(4, target_arch))?,
        state: u8::try_from(state)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(5, state))?,
    })
}

fn validate_retry(
    connection: &rusqlite::Connection,
    plan: ExecutionImagePlan,
    fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
    toolchain_source_digest: [u8; REQUEST_FINGERPRINT_BYTES],
    cargo_source_digest: [u8; REQUEST_FINGERPRINT_BYTES],
) -> Result<(), PersistenceError> {
    let stored = connection.query_row(
        "SELECT operation_fingerprint, toolchain_source_digest, cargo_source_digest
         FROM execution_image_requests WHERE request_id = ?1",
        [&plan.request_id.as_bytes()[..]],
        |row| {
            Ok((
                row.get::<_, [u8; 32]>(0)?,
                row.get::<_, [u8; 32]>(1)?,
                row.get::<_, [u8; 32]>(2)?,
            ))
        },
    )?;
    if stored != (fingerprint, toolchain_source_digest, cargo_source_digest) {
        return Err(PersistenceError::RequestConflict);
    }
    Ok(())
}

fn validate_plan(
    current: ExecutionImagePlan,
    expected: ExecutionImagePlan,
) -> Result<(), PersistenceError> {
    if current.request_id != expected.request_id
        || current.generation_id != expected.generation_id
        || current.operation_id != expected.operation_id
        || current.target_os != expected.target_os
        || current.target_arch != expected.target_arch
    {
        return Err(PersistenceError::InvalidState {
            reason: "an execution image operation changed identity",
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_fact(
    transaction: &Transaction<'_>,
    fact_id: &[u8; 16],
    fact_sequence: u64,
    request_id: MutationRequestId,
    generation_id: &[u8; 16],
    operation_id: &[u8; 16],
    fact_kind: i64,
    outcome: Option<ExecutionImageOutcome>,
    created_at: u64,
) -> Result<(), PersistenceError> {
    let values = outcome.map(|value| {
        (
            value.file_count,
            value.directory_count,
            value.logical_bytes,
            value.manifest_digest,
        )
    });
    transaction.execute(
        "INSERT INTO execution_image_facts (
            fact_id, fact_sequence, request_id, generation_id, operation_id,
            fact_kind, file_count, directory_count, logical_bytes,
            manifest_digest, created_at_milliseconds
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            &fact_id[..],
            sequence_to_sql(fact_sequence)?,
            &request_id.as_bytes()[..],
            &generation_id[..],
            &operation_id[..],
            fact_kind,
            values.map(|value| sequence_to_sql(value.0)).transpose()?,
            values.map(|value| sequence_to_sql(value.1)).transpose()?,
            values.map(|value| sequence_to_sql(value.2)).transpose()?,
            values.map(|value| value.3.to_vec()),
            time_to_sql(created_at)?,
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_audit(
    transaction: &Transaction<'_>,
    audit_id: &[u8; 16],
    audit_sequence: u64,
    request_id: MutationRequestId,
    generation_id: &[u8; 16],
    operation_id: &[u8; 16],
    audit_kind: i64,
    created_at: u64,
) -> Result<(), PersistenceError> {
    transaction.execute(
        "INSERT INTO execution_image_audit_facts (
            audit_id, audit_sequence, request_id, generation_id, operation_id,
            audit_kind, created_at_milliseconds
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            &audit_id[..],
            sequence_to_sql(audit_sequence)?,
            &request_id.as_bytes()[..],
            &generation_id[..],
            &operation_id[..],
            audit_kind,
            time_to_sql(created_at)?,
        ],
    )?;
    Ok(())
}

fn summary(
    state: ExecutionImageState,
    plan: ExecutionImagePlan,
    outcome: Option<ExecutionImageOutcome>,
) -> ExecutionImageSummary {
    ExecutionImageSummary {
        state,
        target_os: plan.target_os,
        target_arch: plan.target_arch,
        format_version: FORMAT_VERSION,
        limits_version: LIMITS_VERSION,
        file_count: outcome.map_or(0, |value| value.file_count),
        logical_bytes: outcome.map_or(0, |value| value.logical_bytes),
    }
}

fn nonnegative(value: i64, column: usize) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(column, value))
}

const fn current_target_os() -> ExecutionTargetOs {
    if cfg!(target_os = "macos") {
        ExecutionTargetOs::Macos
    } else if cfg!(target_os = "linux") {
        ExecutionTargetOs::Linux
    } else {
        ExecutionTargetOs::Windows
    }
}

const fn current_target_arch() -> ExecutionTargetArch {
    if cfg!(target_arch = "aarch64") {
        ExecutionTargetArch::Aarch64
    } else {
        ExecutionTargetArch::X86_64
    }
}
