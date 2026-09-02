use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use super::{
    Backend,
    records::{
        current_time_milliseconds, load_mutation_operation, load_session, next_sequence,
        random_identifier, sequence_to_sql, time_to_sql,
    },
};
use crate::persistence::{
    MutationRequestId, PersistenceError, RepositoryImportOutcome, RepositoryImportPlan, SessionId,
    WorkspaceBlockReason, WorkspaceState, WorkspaceSummary, types::REQUEST_FINGERPRINT_BYTES,
};

pub(super) const MUTATION_OPERATION_IMPORT_REPOSITORY: i64 = 7;
pub(super) const IMPORT_STATE_PREPARED: u8 = 0;
pub(super) const IMPORT_STATE_DISPATCHED: u8 = 1;
pub(super) const IMPORT_STATE_READY: u8 = 2;
pub(super) const IMPORT_STATE_NOT_APPLIED: u8 = 3;
pub(super) const IMPORT_STATE_BLOCKED: u8 = 4;
const FACT_PREPARED: i64 = 1;
const FACT_DISPATCHED: i64 = 2;
const FACT_COMPLETED: i64 = 3;
const FACT_NOT_APPLIED: i64 = 4;
const FACT_BLOCKED: i64 = 5;
const EVENT_WORKSPACE_CHANGED: i64 = 11;
const MAX_REPOSITORY_IMPORT_REQUESTS: i64 = 10_000;

impl Backend {
    pub(super) fn recover_repository_imports(&mut self) -> Result<(), PersistenceError> {
        let plans = self.repository_imports_for_recovery()?;
        let paths = self.paths.clone();
        for plan in plans {
            if plan.state == IMPORT_STATE_PREPARED {
                self.mark_repository_import_not_applied(plan)?;
                continue;
            }
            match paths.recover_repository_import(plan)? {
                crate::persistence::workspace::RepositoryRecovery::Complete(outcome) => {
                    self.complete_repository_import(plan, outcome)?;
                }
                crate::persistence::workspace::RepositoryRecovery::NotApplied => {
                    self.mark_repository_import_not_applied(plan)?;
                }
                crate::persistence::workspace::RepositoryRecovery::Blocked => {
                    self.block_repository_import(plan)?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn validate_ready_repositories(&self) -> Result<(), PersistenceError> {
        let paths = self.paths.clone();
        for (plan, outcome) in self.ready_repository_imports()? {
            paths.validate_completed_repository(plan, outcome)?;
        }
        Ok(())
    }

    pub(crate) fn prepare_repository_import(
        &mut self,
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        source_path_digest: [u8; REQUEST_FINGERPRINT_BYTES],
        session_id: SessionId,
    ) -> Result<RepositoryImportPlan, PersistenceError> {
        if let Some(existing) = load_import_plan(&self.connection, request_id)? {
            validate_retry(
                &self.connection,
                &existing,
                fingerprint,
                source_path_digest,
                session_id,
            )?;
            return Ok(existing);
        }
        if load_mutation_operation(&self.connection, request_id)?.is_some() {
            return Err(PersistenceError::RequestConflict);
        }

        let session =
            load_session(&self.connection, session_id)?.ok_or(PersistenceError::SessionNotFound)?;
        let operation_id = random_identifier()?;
        let generation_id = random_identifier()?;
        let generation_operation_id = random_identifier()?;
        let fact_id = random_identifier()?;
        let event_id = random_identifier()?;
        let audit_id = random_identifier()?;
        let accepted_at_milliseconds = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_pristine_workspace(&transaction, session_id)?;
        let request_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM repository_import_requests",
            [],
            |row| row.get(0),
        )?;
        if request_count >= MAX_REPOSITORY_IMPORT_REQUESTS {
            return Err(PersistenceError::ResourceLimit {
                resource: crate::persistence::PersistenceResourceLimit::Workspace,
            });
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
                MUTATION_OPERATION_IMPORT_REPOSITORY,
                sequence_to_sql(accepted_sequence)?,
                time_to_sql(accepted_at_milliseconds)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO repository_import_requests (
                request_id, operation_fingerprint, source_path_digest, session_id,
                workspace_id, operation_id, accepted_sequence, accepted_at_milliseconds, state,
                review_baseline_version
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1)",
            params![
                &request_id.as_bytes()[..],
                &fingerprint[..],
                &source_path_digest[..],
                &session_id.as_bytes()[..],
                &session.workspace_id[..],
                &operation_id[..],
                sequence_to_sql(accepted_sequence)?,
                time_to_sql(accepted_at_milliseconds)?,
                i64::from(IMPORT_STATE_PREPARED),
            ],
        )?;
        transaction.execute(
            "INSERT INTO workspace_generation_layouts (
                workspace_id, session_id, import_request_id, generation_id, operation_id,
                state, created_sequence, updated_sequence
             ) VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?6)",
            params![
                &session.workspace_id[..],
                &session_id.as_bytes()[..],
                &request_id.as_bytes()[..],
                &generation_id[..],
                &generation_operation_id[..],
                sequence_to_sql(accepted_sequence)?,
            ],
        )?;
        insert_fact(
            &transaction,
            &fact_id,
            fact_sequence,
            request_id,
            session_id,
            &session.workspace_id,
            &operation_id,
            FACT_PREPARED,
            None,
            Some(&event_id),
            accepted_at_milliseconds,
        )?;
        insert_audit(
            &transaction,
            &audit_id,
            audit_sequence,
            request_id,
            session_id,
            &operation_id,
            FACT_PREPARED,
            accepted_at_milliseconds,
        )?;
        insert_workspace_event(
            &transaction,
            &event_id,
            fact_sequence,
            session_id,
            accepted_at_milliseconds,
        )?;
        update_session_sequence(&transaction, session_id, fact_sequence)?;
        transaction.commit()?;
        Ok(RepositoryImportPlan {
            request_id,
            session_id,
            workspace_id: session.workspace_id,
            operation_id,
            generation_id,
            state: IMPORT_STATE_PREPARED,
        })
    }

    pub(crate) fn dispatch_repository_import(
        &mut self,
        expected: RepositoryImportPlan,
    ) -> Result<RepositoryImportPlan, PersistenceError> {
        let current = load_import_plan(&self.connection, expected.request_id)?.ok_or(
            PersistenceError::InvalidState {
                reason: "a repository import request disappeared before dispatch",
            },
        )?;
        validate_plan(&current, &expected)?;
        if current.state != IMPORT_STATE_PREPARED {
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
            current.session_id,
            &current.workspace_id,
            &current.operation_id,
            FACT_DISPATCHED,
            None,
            None,
            created_at,
        )?;
        insert_audit(
            &transaction,
            &audit_id,
            audit_sequence,
            current.request_id,
            current.session_id,
            &current.operation_id,
            FACT_DISPATCHED,
            created_at,
        )?;
        let updated = transaction.execute(
            "UPDATE repository_import_requests SET state = ?2
             WHERE request_id = ?1 AND state = ?3",
            params![
                &current.request_id.as_bytes()[..],
                i64::from(IMPORT_STATE_DISPATCHED),
                i64::from(IMPORT_STATE_PREPARED),
            ],
        )?;
        let layout_updated = transaction.execute(
            "UPDATE workspace_generation_layouts SET state = 1, updated_sequence = ?2
             WHERE import_request_id = ?1 AND state = 0",
            params![
                &current.request_id.as_bytes()[..],
                sequence_to_sql(fact_sequence)?,
            ],
        )?;
        if updated != 1 || layout_updated != 1 {
            return Err(PersistenceError::InvalidState {
                reason: "a repository import did not reach dispatched generation state",
            });
        }
        transaction.commit()?;
        Ok(RepositoryImportPlan {
            state: IMPORT_STATE_DISPATCHED,
            ..current
        })
    }

    pub(crate) fn complete_repository_import(
        &mut self,
        expected: RepositoryImportPlan,
        outcome: RepositoryImportOutcome,
    ) -> Result<WorkspaceSummary, PersistenceError> {
        self.finalize_repository_import(expected, IMPORT_STATE_READY, Some(outcome))
    }

    pub(crate) fn mark_repository_import_not_applied(
        &mut self,
        expected: RepositoryImportPlan,
    ) -> Result<WorkspaceSummary, PersistenceError> {
        self.finalize_repository_import(expected, IMPORT_STATE_NOT_APPLIED, None)
    }

    pub(crate) fn block_repository_import(
        &mut self,
        expected: RepositoryImportPlan,
    ) -> Result<WorkspaceSummary, PersistenceError> {
        self.finalize_repository_import(expected, IMPORT_STATE_BLOCKED, None)
    }

    fn ready_repository_imports(
        &self,
    ) -> Result<Vec<(RepositoryImportPlan, RepositoryImportOutcome)>, PersistenceError> {
        let mut statement = self.connection.prepare(
            "SELECT request.request_id, request.session_id, request.workspace_id,
                    request.operation_id, layout.generation_id, request.state,
                    request.file_count, request.directory_count,
                    request.logical_bytes, request.manifest_digest
             FROM repository_import_requests AS request
             JOIN workspace_generation_layouts AS layout
               ON layout.import_request_id = request.request_id
             WHERE request.state = 2 ORDER BY request.accepted_sequence",
        )?;
        statement
            .query_map([], |row| {
                let plan = import_plan_from_row(row)?;
                let file_count = row.get::<_, i64>(6)?;
                let directory_count = row.get::<_, i64>(7)?;
                let logical_bytes = row.get::<_, i64>(8)?;
                Ok((
                    plan,
                    RepositoryImportOutcome {
                        file_count: u64::try_from(file_count)
                            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(6, file_count))?,
                        directory_count: u64::try_from(directory_count).map_err(|_| {
                            rusqlite::Error::IntegralValueOutOfRange(7, directory_count)
                        })?,
                        logical_bytes: u64::try_from(logical_bytes).map_err(|_| {
                            rusqlite::Error::IntegralValueOutOfRange(8, logical_bytes)
                        })?,
                        manifest_digest: row.get(9)?,
                    },
                ))
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(PersistenceError::from)
    }

    pub(crate) fn repository_imports_for_recovery(
        &self,
    ) -> Result<Vec<RepositoryImportPlan>, PersistenceError> {
        let mut statement = self.connection.prepare(
            "SELECT request.request_id, request.session_id, request.workspace_id,
                    request.operation_id, layout.generation_id, request.state
             FROM repository_import_requests AS request
             JOIN workspace_generation_layouts AS layout
               ON layout.import_request_id = request.request_id
             WHERE request.state IN (0, 1)
             ORDER BY request.accepted_sequence",
        )?;
        statement
            .query_map([], import_plan_from_row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(PersistenceError::from)
    }

    pub(crate) fn workspace_summary(
        &self,
        session_id: SessionId,
    ) -> Result<WorkspaceSummary, PersistenceError> {
        if load_session(&self.connection, session_id)?.is_none() {
            return Err(PersistenceError::SessionNotFound);
        }
        workspace_summary_at_sequence(&self.connection, session_id, u64::MAX)
    }

    fn finalize_repository_import(
        &mut self,
        expected: RepositoryImportPlan,
        target_state: u8,
        outcome: Option<RepositoryImportOutcome>,
    ) -> Result<WorkspaceSummary, PersistenceError> {
        let current = load_import_plan(&self.connection, expected.request_id)?.ok_or(
            PersistenceError::InvalidState {
                reason: "a repository import request disappeared before completion",
            },
        )?;
        validate_plan(&current, &expected)?;
        if current.state == target_state {
            return self.workspace_summary(current.session_id);
        }
        if current.state != IMPORT_STATE_DISPATCHED
            && !(current.state == IMPORT_STATE_PREPARED && target_state == IMPORT_STATE_NOT_APPLIED)
        {
            return Err(PersistenceError::InvalidState {
                reason: "a repository import has an invalid terminal transition",
            });
        }
        if (target_state == IMPORT_STATE_READY) != outcome.is_some()
            || !matches!(
                target_state,
                IMPORT_STATE_READY | IMPORT_STATE_NOT_APPLIED | IMPORT_STATE_BLOCKED
            )
        {
            return Err(PersistenceError::InvalidState {
                reason: "a repository import terminal outcome is invalid",
            });
        }

        let fact_kind = match target_state {
            IMPORT_STATE_READY => FACT_COMPLETED,
            IMPORT_STATE_NOT_APPLIED => FACT_NOT_APPLIED,
            IMPORT_STATE_BLOCKED => FACT_BLOCKED,
            _ => unreachable!(),
        };
        let fact_id = random_identifier()?;
        let event_id = random_identifier()?;
        let audit_id = random_identifier()?;
        let generation_fact_id = random_identifier()?;
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
            current.session_id,
            &current.workspace_id,
            &current.operation_id,
            fact_kind,
            outcome,
            Some(&event_id),
            created_at,
        )?;
        insert_audit(
            &transaction,
            &audit_id,
            audit_sequence,
            current.request_id,
            current.session_id,
            &current.operation_id,
            fact_kind,
            created_at,
        )?;
        let (file_count, directory_count, logical_bytes, manifest_digest) =
            outcome.map_or((None, None, None, None), |value| {
                (
                    Some(value.file_count),
                    Some(value.directory_count),
                    Some(value.logical_bytes),
                    Some(value.manifest_digest),
                )
            });
        let updated = transaction.execute(
            "UPDATE repository_import_requests
             SET state = ?2, file_count = ?3, directory_count = ?4,
                 logical_bytes = ?5, manifest_digest = ?6
             WHERE request_id = ?1 AND state = ?7",
            params![
                &current.request_id.as_bytes()[..],
                i64::from(target_state),
                file_count.map(sequence_to_sql).transpose()?,
                directory_count.map(sequence_to_sql).transpose()?,
                logical_bytes.map(sequence_to_sql).transpose()?,
                manifest_digest.as_ref().map(|digest| &digest[..]),
                i64::from(current.state),
            ],
        )?;
        let layout = transaction.query_row(
            "SELECT operation_id, state FROM workspace_generation_layouts
             WHERE import_request_id = ?1 AND generation_id = ?2",
            params![
                &current.request_id.as_bytes()[..],
                &current.generation_id[..],
            ],
            |row| Ok((row.get::<_, [u8; 16]>(0)?, row.get::<_, i64>(1)?)),
        )?;
        let layout_target = if target_state == IMPORT_STATE_READY {
            2
        } else {
            3
        };
        let layout_updated = transaction.execute(
            "UPDATE workspace_generation_layouts
             SET state = ?3, updated_sequence = ?4,
                 file_count = ?5, directory_count = ?6,
                 logical_bytes = ?7, manifest_digest = ?8
             WHERE import_request_id = ?1 AND generation_id = ?2 AND state = ?9",
            params![
                &current.request_id.as_bytes()[..],
                &current.generation_id[..],
                layout_target,
                sequence_to_sql(fact_sequence)?,
                file_count.map(sequence_to_sql).transpose()?,
                directory_count.map(sequence_to_sql).transpose()?,
                logical_bytes.map(sequence_to_sql).transpose()?,
                manifest_digest.as_ref().map(|digest| &digest[..]),
                layout.1,
            ],
        )?;
        if updated != 1 || layout_updated != 1 {
            return Err(PersistenceError::InvalidState {
                reason: "a repository import did not reach terminal generation state",
            });
        }
        if let Some(outcome) = outcome {
            transaction.execute(
                "INSERT INTO worktree_generation_facts (
                    fact_id, fact_sequence, session_id, workspace_id, generation_id,
                    predecessor_generation_id, publication_kind, operation_id,
                    file_count, directory_count, logical_bytes, manifest_digest,
                    created_at_milliseconds
                 ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, 1, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    &generation_fact_id[..],
                    sequence_to_sql(fact_sequence)?,
                    &current.session_id.as_bytes()[..],
                    &current.workspace_id[..],
                    &current.generation_id[..],
                    &layout.0[..],
                    sequence_to_sql(outcome.file_count)?,
                    sequence_to_sql(outcome.directory_count)?,
                    sequence_to_sql(outcome.logical_bytes)?,
                    &outcome.manifest_digest[..],
                    time_to_sql(created_at)?,
                ],
            )?;
            transaction.execute(
                "INSERT INTO active_worktree_generations (
                    workspace_id, session_id, generation_id, updated_sequence
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    &current.workspace_id[..],
                    &current.session_id.as_bytes()[..],
                    &current.generation_id[..],
                    sequence_to_sql(fact_sequence)?,
                ],
            )?;
        }
        insert_workspace_event(
            &transaction,
            &event_id,
            fact_sequence,
            current.session_id,
            created_at,
        )?;
        update_session_sequence(&transaction, current.session_id, fact_sequence)?;
        transaction.commit()?;
        self.workspace_summary(current.session_id)
    }
}

pub(super) fn workspace_summary_at_sequence(
    connection: &Connection,
    session_id: SessionId,
    event_sequence: u64,
) -> Result<WorkspaceSummary, PersistenceError> {
    let event_sequence = if event_sequence == u64::MAX {
        i64::MAX
    } else {
        sequence_to_sql(event_sequence)?
    };
    let record = connection
        .query_row(
            "SELECT fact_kind, file_count, logical_bytes
             FROM repository_import_facts
             WHERE session_id = ?1 AND fact_sequence <= ?2 AND fact_kind IN (1, 3, 4, 5)
             ORDER BY fact_sequence DESC LIMIT 1",
            params![&session_id.as_bytes()[..], event_sequence],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            },
        )
        .optional()?;
    let summary = match record {
        None | Some((FACT_NOT_APPLIED, None, None)) => empty_workspace(),
        Some((FACT_PREPARED, None, None)) => WorkspaceSummary {
            state: WorkspaceState::Importing,
            file_count: 0,
            logical_bytes: 0,
            block_reason: None,
            blocked_run_id: None,
            blocked_tool: None,
        },
        Some((FACT_COMPLETED, Some(file_count), Some(logical_bytes))) => WorkspaceSummary {
            state: WorkspaceState::Ready,
            file_count: nonnegative_u64(file_count)?,
            logical_bytes: nonnegative_u64(logical_bytes)?,
            block_reason: None,
            blocked_run_id: None,
            blocked_tool: None,
        },
        Some((FACT_BLOCKED, None, None)) => WorkspaceSummary {
            state: WorkspaceState::Blocked,
            file_count: 0,
            logical_bytes: 0,
            block_reason: Some(WorkspaceBlockReason::InconsistentImportState),
            blocked_run_id: None,
            blocked_tool: None,
        },
        _ => {
            return Err(PersistenceError::InvalidState {
                reason: "repository import facts have an invalid workspace summary",
            });
        }
    };
    if summary.state != WorkspaceState::Ready {
        return Ok(summary);
    }
    let uncertainty = connection
        .query_row(
            "SELECT uncertain.run_id, call.tool_kind
             FROM tool_operation_facts AS uncertain
             JOIN tool_calls AS call ON call.call_id = uncertain.call_id
             WHERE uncertain.session_id = ?1
               AND uncertain.fact_kind = 6
               AND uncertain.fact_sequence <= ?2
               AND NOT EXISTS (
                   SELECT 1 FROM tool_uncertainty_acknowledgements AS acknowledgement
                   WHERE acknowledgement.run_id = uncertain.run_id
                     AND acknowledgement.fact_sequence <= ?2
               )
             ORDER BY uncertain.fact_sequence DESC LIMIT 1",
            params![&session_id.as_bytes()[..], event_sequence],
            |row| Ok((row.get::<_, [u8; 16]>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;
    let Some((run_id, tool_kind)) = uncertainty else {
        return Ok(summary);
    };
    let tool =
        crate::tools::ToolKind::from_record(tool_kind).ok_or(PersistenceError::InvalidState {
            reason: "an uncertain tool effect has an invalid tool kind",
        })?;
    Ok(WorkspaceSummary {
        state: WorkspaceState::Blocked,
        block_reason: Some(WorkspaceBlockReason::UncertainToolEffect),
        blocked_run_id: Some(crate::persistence::RunId::from_bytes(run_id)),
        blocked_tool: Some(tool),
        ..summary
    })
}

pub(super) fn workspace_summary_for_event(
    connection: &Connection,
    session_id: SessionId,
    event_id: &[u8; 16],
    event_sequence: u64,
) -> Result<WorkspaceSummary, PersistenceError> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM repository_import_facts
            WHERE session_id = ?1 AND delivery_event_id = ?2
              AND fact_sequence = ?3 AND fact_kind IN (1, 3, 4, 5)
            UNION ALL
            SELECT 1 FROM tool_operation_facts
            WHERE session_id = ?1 AND workspace_delivery_event_id = ?2
              AND fact_sequence = ?3 AND fact_kind = 6
            UNION ALL
            SELECT 1 FROM tool_uncertainty_acknowledgements
            WHERE session_id = ?1 AND delivery_event_id = ?2 AND fact_sequence = ?3
        )",
        params![
            &session_id.as_bytes()[..],
            &event_id[..],
            sequence_to_sql(event_sequence)?,
        ],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(PersistenceError::InvalidState {
            reason: "a workspace event is missing its canonical import fact",
        });
    }
    workspace_summary_at_sequence(connection, session_id, event_sequence)
}

fn ensure_pristine_workspace(
    transaction: &Transaction<'_>,
    session_id: SessionId,
) -> Result<(), PersistenceError> {
    let (entry_count, run_count): (i64, i64) = transaction.query_row(
        "SELECT
            (SELECT COUNT(*) FROM session_entries WHERE session_id = ?1),
            (SELECT COUNT(*) FROM run_accepted_facts WHERE session_id = ?1)",
        [&session_id.as_bytes()[..]],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if entry_count != 0 || run_count != 0 {
        return Err(PersistenceError::WorkspaceNotPristine);
    }
    let state = transaction
        .query_row(
            "SELECT state FROM repository_import_requests
             WHERE session_id = ?1 AND state IN (0, 1, 2, 4)
             ORDER BY accepted_sequence DESC LIMIT 1",
            [&session_id.as_bytes()[..]],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    match state {
        None => Ok(()),
        Some(0 | 1) => Err(PersistenceError::WorkspaceBusy),
        Some(2) => Err(PersistenceError::RepositoryAlreadyImported),
        Some(4) => Err(PersistenceError::WorkspaceBlocked),
        _ => Err(PersistenceError::InvalidState {
            reason: "a repository import projection has an invalid active state",
        }),
    }
}

fn load_import_plan(
    connection: &Connection,
    request_id: MutationRequestId,
) -> Result<Option<RepositoryImportPlan>, PersistenceError> {
    connection
        .query_row(
            "SELECT request.request_id, request.session_id, request.workspace_id,
                    request.operation_id, layout.generation_id, request.state
             FROM repository_import_requests AS request
             JOIN workspace_generation_layouts AS layout
               ON layout.import_request_id = request.request_id
             WHERE request.request_id = ?1",
            [&request_id.as_bytes()[..]],
            import_plan_from_row,
        )
        .optional()
        .map_err(PersistenceError::from)
}

fn import_plan_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RepositoryImportPlan> {
    let state = row.get::<_, i64>(5)?;
    Ok(RepositoryImportPlan {
        request_id: MutationRequestId::from_bytes(row.get(0)?),
        session_id: SessionId::from_bytes(row.get(1)?),
        workspace_id: row.get(2)?,
        operation_id: row.get(3)?,
        generation_id: row.get(4)?,
        state: u8::try_from(state)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(5, state))?,
    })
}

fn validate_retry(
    connection: &Connection,
    plan: &RepositoryImportPlan,
    fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
    source_path_digest: [u8; REQUEST_FINGERPRINT_BYTES],
    session_id: SessionId,
) -> Result<(), PersistenceError> {
    let stored = connection.query_row(
        "SELECT operation_fingerprint, source_path_digest
         FROM repository_import_requests WHERE request_id = ?1",
        [&plan.request_id.as_bytes()[..]],
        |row| {
            Ok((
                row.get::<_, [u8; REQUEST_FINGERPRINT_BYTES]>(0)?,
                row.get::<_, [u8; REQUEST_FINGERPRINT_BYTES]>(1)?,
            ))
        },
    )?;
    if stored != (fingerprint, source_path_digest) || plan.session_id != session_id {
        return Err(PersistenceError::RequestConflict);
    }
    Ok(())
}

fn validate_plan(
    current: &RepositoryImportPlan,
    expected: &RepositoryImportPlan,
) -> Result<(), PersistenceError> {
    if current.request_id != expected.request_id
        || current.session_id != expected.session_id
        || current.workspace_id != expected.workspace_id
        || current.operation_id != expected.operation_id
        || current.generation_id != expected.generation_id
    {
        return Err(PersistenceError::InvalidState {
            reason: "a repository import identity changed",
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
    session_id: SessionId,
    workspace_id: &[u8; 16],
    operation_id: &[u8; 16],
    fact_kind: i64,
    outcome: Option<RepositoryImportOutcome>,
    delivery_event_id: Option<&[u8; 16]>,
    created_at: u64,
) -> Result<(), PersistenceError> {
    let (file_count, directory_count, logical_bytes, manifest_digest) =
        outcome.map_or((None, None, None, None), |value| {
            (
                Some(value.file_count),
                Some(value.directory_count),
                Some(value.logical_bytes),
                Some(value.manifest_digest),
            )
        });
    transaction.execute(
        "INSERT INTO repository_import_facts (
            fact_id, fact_sequence, request_id, session_id, workspace_id, operation_id,
            fact_kind, file_count, directory_count, logical_bytes, manifest_digest,
            delivery_event_id, created_at_milliseconds
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            &fact_id[..],
            sequence_to_sql(fact_sequence)?,
            &request_id.as_bytes()[..],
            &session_id.as_bytes()[..],
            &workspace_id[..],
            &operation_id[..],
            fact_kind,
            file_count.map(sequence_to_sql).transpose()?,
            directory_count.map(sequence_to_sql).transpose()?,
            logical_bytes.map(sequence_to_sql).transpose()?,
            manifest_digest.as_ref().map(|digest| &digest[..]),
            delivery_event_id.map(|event_id| &event_id[..]),
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
    session_id: SessionId,
    operation_id: &[u8; 16],
    audit_kind: i64,
    created_at: u64,
) -> Result<(), PersistenceError> {
    transaction.execute(
        "INSERT INTO repository_import_audit_facts (
            audit_id, audit_sequence, request_id, session_id, operation_id,
            audit_kind, created_at_milliseconds
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            &audit_id[..],
            sequence_to_sql(audit_sequence)?,
            &request_id.as_bytes()[..],
            &session_id.as_bytes()[..],
            &operation_id[..],
            audit_kind,
            time_to_sql(created_at)?,
        ],
    )?;
    Ok(())
}

fn insert_workspace_event(
    transaction: &Transaction<'_>,
    event_id: &[u8; 16],
    event_sequence: u64,
    session_id: SessionId,
    created_at: u64,
) -> Result<(), PersistenceError> {
    transaction.execute(
        "INSERT INTO delivery_events (
            event_id, event_sequence, session_id, event_kind,
            payload_version, created_at_milliseconds
         ) VALUES (?1, ?2, ?3, ?4, 1, ?5)",
        params![
            &event_id[..],
            sequence_to_sql(event_sequence)?,
            &session_id.as_bytes()[..],
            EVENT_WORKSPACE_CHANGED,
            time_to_sql(created_at)?,
        ],
    )?;
    Ok(())
}

fn update_session_sequence(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    sequence: u64,
) -> Result<(), PersistenceError> {
    let sequence = sequence_to_sql(sequence)?;
    let updated = transaction.execute(
        "UPDATE sessions SET updated_sequence = ?2 WHERE session_id = ?1",
        params![&session_id.as_bytes()[..], sequence],
    )?;
    let run_state_updated = transaction.execute(
        "UPDATE session_run_states SET updated_sequence = ?2 WHERE session_id = ?1",
        params![&session_id.as_bytes()[..], sequence],
    )?;
    if updated != 1 || run_state_updated != 1 {
        return Err(PersistenceError::InvalidState {
            reason: "a repository import could not update its session projection",
        });
    }
    Ok(())
}

fn empty_workspace() -> WorkspaceSummary {
    WorkspaceSummary {
        state: WorkspaceState::Empty,
        file_count: 0,
        logical_bytes: 0,
        block_reason: None,
        blocked_run_id: None,
        blocked_tool: None,
    }
}

fn nonnegative_u64(value: i64) -> Result<u64, PersistenceError> {
    u64::try_from(value).map_err(|_| PersistenceError::InvalidState {
        reason: "a repository import count is invalid",
    })
}
