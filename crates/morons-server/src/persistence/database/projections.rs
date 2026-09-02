mod rebuild;

use rusqlite::{Connection, params};

use super::validate_integrity;
use crate::persistence::{
    PersistenceError, RunModelSelection, RunOpenCodeService, SessionId,
    run_types::{CONTEXT_POLICY_VERSION, MAX_CONTEXT_ENTRIES, conservative_input_token_estimate},
    types::{
        acknowledge_tool_uncertainty_fingerprint, cancel_run_fingerprint,
        create_session_fingerprint, import_repository_fingerprint_from_digest,
        provision_execution_image_fingerprint, stop_server_fingerprint,
        submit_session_input_fingerprint, validate_display_name, validate_model_selection,
        validate_user_text,
    },
};
use crate::tools::{
    ToolErrorKind, ToolInput, ToolKind, ToolResult, recovery_plan_is_valid, tool_path_digest,
    validate_canonical_input, validate_canonical_result,
};

pub(super) fn repair(connection: &mut Connection) -> Result<(), PersistenceError> {
    validate_session_creation_facts(connection)?;
    validate_mutation_registry(connection)?;
    validate_run_request_payloads(connection)?;
    validate_server_stop_facts(connection)?;
    validate_repository_import_facts(connection)?;
    validate_execution_image_facts(connection)?;
    validate_run_canonical_facts(connection)?;
    validate_tool_facts(connection)?;
    validate_logical_sequences(connection)?;
    rebuild::rebuild(connection)?;
    validate_integrity(connection)
}

fn validate_session_creation_facts(connection: &Connection) -> Result<(), PersistenceError> {
    let requests_invalid: bool = connection.query_row(
        "SELECT EXISTS (
            SELECT 1
            FROM session_creation_requests AS request
            LEFT JOIN session_created_facts AS fact ON fact.request_id = request.request_id
            WHERE (fact.request_id IS NOT NULL) != (request.state = 2)
               OR (fact.request_id IS NOT NULL AND (
                    fact.session_id IS NOT request.session_id
                    OR fact.workspace_id IS NOT request.workspace_id
                    OR fact.display_name IS NOT request.display_name
                    OR fact.accepted_sequence IS NOT request.accepted_sequence
                    OR fact.created_at_milliseconds IS NOT request.accepted_at_milliseconds
               ))
               OR (SELECT COUNT(*) FROM workspace_operation_facts AS workspace
                   WHERE workspace.request_id = request.request_id
                     AND workspace.operation_kind = 1) != CASE WHEN request.state >= 1 THEN 1 ELSE 0 END
               OR (SELECT COUNT(*) FROM workspace_operation_facts AS workspace
                   WHERE workspace.request_id = request.request_id
                     AND workspace.operation_kind = 2) != CASE WHEN request.state = 2 THEN 1 ELSE 0 END
               OR EXISTS (
                    SELECT 1 FROM workspace_operation_facts AS workspace
                    WHERE workspace.request_id = request.request_id
                      AND workspace.workspace_id IS NOT request.workspace_id
               )
               OR (SELECT COUNT(*) FROM audit_facts AS audit
                   WHERE audit.request_id = request.request_id
                     AND audit.audit_kind = 1) != 1
               OR (SELECT COUNT(*) FROM audit_facts AS audit
                   WHERE audit.request_id = request.request_id
                     AND audit.audit_kind = 2) != CASE WHEN request.state >= 1 THEN 1 ELSE 0 END
               OR (SELECT COUNT(*) FROM audit_facts AS audit
                   WHERE audit.request_id = request.request_id
                     AND audit.audit_kind = 3) != CASE WHEN request.state = 2 THEN 1 ELSE 0 END
               OR EXISTS (
                    SELECT 1 FROM audit_facts AS audit
                    WHERE audit.request_id = request.request_id
                      AND audit.session_id IS NOT request.session_id
               )
        )",
        [],
        |row| row.get(0),
    )?;
    if requests_invalid {
        return Err(PersistenceError::InvalidState {
            reason: "session creation facts conflict with idempotency state",
        });
    }

    let mut statement = connection
        .prepare("SELECT operation_fingerprint, display_name FROM session_creation_requests")?;
    let requests = statement
        .query_map([], |row| {
            Ok((row.get::<_, [u8; 32]>(0)?, row.get::<_, Option<String>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (fingerprint, display_name) in requests {
        if validate_display_name(display_name.as_deref()).is_err()
            || fingerprint != create_session_fingerprint(display_name.as_deref())
        {
            return Err(PersistenceError::InvalidState {
                reason: "a persisted session creation request has invalid canonical input",
            });
        }
    }
    Ok(())
}

fn validate_mutation_registry(connection: &Connection) -> Result<(), PersistenceError> {
    let invalid: bool = connection.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM mutation_requests AS mutation
            LEFT JOIN session_creation_requests AS request
              ON request.request_id = mutation.request_id
            WHERE mutation.operation_kind = 1 AND (
                request.request_id IS NULL
                OR mutation.accepted_sequence IS NOT request.accepted_sequence
                OR mutation.accepted_at_milliseconds IS NOT request.accepted_at_milliseconds
            )
            UNION ALL
            SELECT 1 FROM mutation_requests AS mutation
            LEFT JOIN credential_mutation_requests AS request
              ON request.request_id = mutation.request_id
            WHERE mutation.operation_kind IN (2, 3) AND (
                request.request_id IS NULL
                OR mutation.operation_kind IS NOT request.operation_kind
                OR mutation.accepted_sequence IS NOT request.accepted_sequence
                OR mutation.accepted_at_milliseconds IS NOT request.accepted_at_milliseconds
            )
            UNION ALL
            SELECT 1 FROM mutation_requests AS mutation
            LEFT JOIN run_input_requests AS request ON request.request_id = mutation.request_id
            WHERE mutation.operation_kind = 4 AND (
                request.request_id IS NULL
                OR mutation.accepted_sequence IS NOT request.accepted_sequence
                OR mutation.accepted_at_milliseconds IS NOT request.accepted_at_milliseconds
            )
            UNION ALL
            SELECT 1 FROM mutation_requests AS mutation
            LEFT JOIN run_cancellation_requests AS request
              ON request.request_id = mutation.request_id
            WHERE mutation.operation_kind = 5 AND (
                request.request_id IS NULL
                OR mutation.accepted_sequence IS NOT request.fact_sequence
                OR mutation.accepted_at_milliseconds IS NOT request.accepted_at_milliseconds
            )
            UNION ALL
            SELECT 1 FROM mutation_requests AS mutation
            LEFT JOIN server_stop_requests AS request
              ON request.request_id = mutation.request_id
            WHERE mutation.operation_kind = 6 AND (
                request.request_id IS NULL
                OR mutation.accepted_sequence IS NOT request.accepted_sequence
                OR mutation.accepted_at_milliseconds IS NOT request.accepted_at_milliseconds
            )
            UNION ALL
            SELECT 1 FROM mutation_requests AS mutation
            LEFT JOIN tool_uncertainty_acknowledgements AS acknowledgement
              ON acknowledgement.request_id = mutation.request_id
            WHERE mutation.operation_kind = 8 AND (
                acknowledgement.request_id IS NULL
                OR mutation.accepted_sequence IS NOT acknowledgement.fact_sequence
                OR mutation.accepted_at_milliseconds IS NOT acknowledgement.accepted_at_milliseconds
            )
            UNION ALL
            SELECT 1 FROM mutation_requests AS mutation
            LEFT JOIN execution_image_requests AS request
              ON request.request_id = mutation.request_id
            WHERE mutation.operation_kind = 9 AND (
                request.request_id IS NULL
                OR mutation.accepted_sequence IS NOT request.accepted_sequence
                OR mutation.accepted_at_milliseconds IS NOT request.accepted_at_milliseconds
            )
            UNION ALL
            SELECT 1 FROM session_creation_requests AS request
            LEFT JOIN mutation_requests AS mutation ON mutation.request_id = request.request_id
            WHERE mutation.operation_kind IS NOT 1
            UNION ALL
            SELECT 1 FROM credential_mutation_requests AS request
            LEFT JOIN mutation_requests AS mutation ON mutation.request_id = request.request_id
            WHERE mutation.operation_kind IS NOT request.operation_kind
            UNION ALL
            SELECT 1 FROM run_input_requests AS request
            LEFT JOIN mutation_requests AS mutation ON mutation.request_id = request.request_id
            WHERE mutation.operation_kind IS NOT 4
            UNION ALL
            SELECT 1 FROM run_cancellation_requests AS request
            LEFT JOIN mutation_requests AS mutation ON mutation.request_id = request.request_id
            WHERE mutation.operation_kind IS NOT 5
            UNION ALL
            SELECT 1 FROM server_stop_requests AS request
            LEFT JOIN mutation_requests AS mutation ON mutation.request_id = request.request_id
            WHERE mutation.operation_kind IS NOT 6
            UNION ALL
            SELECT 1 FROM tool_uncertainty_acknowledgements AS acknowledgement
            LEFT JOIN mutation_requests AS mutation ON mutation.request_id = acknowledgement.request_id
            WHERE mutation.operation_kind IS NOT 8
            UNION ALL
            SELECT 1 FROM execution_image_requests AS request
            LEFT JOIN mutation_requests AS mutation ON mutation.request_id = request.request_id
            WHERE mutation.operation_kind IS NOT 9
        )",
        [],
        |row| row.get(0),
    )?;
    if invalid {
        return Err(PersistenceError::InvalidState {
            reason: "mutation requests conflict with their canonical operation records",
        });
    }
    Ok(())
}

fn validate_server_stop_facts(connection: &Connection) -> Result<(), PersistenceError> {
    let invalid: bool = connection.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM server_stop_requests AS request
            WHERE (SELECT COUNT(*) FROM server_audit_facts AS audit
                   WHERE audit.request_id = request.request_id
                     AND audit.audit_kind = 1
                     AND audit.host_epoch IS request.host_epoch
                     AND audit.created_at_milliseconds IS request.accepted_at_milliseconds) != 1
            UNION ALL
            SELECT 1 FROM server_audit_facts AS audit
            LEFT JOIN server_stop_requests AS request ON request.request_id = audit.request_id
            WHERE request.request_id IS NULL OR audit.host_epoch IS NOT request.host_epoch
            UNION ALL
            SELECT 1 FROM server_stop_requests
            GROUP BY host_epoch
            HAVING SUM(signal_applied) != 1
        )",
        [],
        |row| row.get(0),
    )?;
    if invalid {
        return Err(PersistenceError::InvalidState {
            reason: "server stop audit facts conflict with idempotency state",
        });
    }
    let mut statement =
        connection.prepare("SELECT operation_fingerprint FROM server_stop_requests")?;
    let fingerprints = statement
        .query_map([], |row| row.get::<_, [u8; 32]>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if fingerprints
        .into_iter()
        .any(|fingerprint| fingerprint != stop_server_fingerprint())
    {
        return Err(PersistenceError::InvalidState {
            reason: "a persisted server stop request has invalid canonical input",
        });
    }
    Ok(())
}

fn validate_repository_import_facts(connection: &Connection) -> Result<(), PersistenceError> {
    let invalid: bool = connection.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM repository_import_requests AS request
            LEFT JOIN mutation_requests AS mutation ON mutation.request_id = request.request_id
            WHERE (mutation.operation_kind IS NOT 7
               OR mutation.accepted_sequence IS NOT request.accepted_sequence
               OR mutation.accepted_at_milliseconds IS NOT request.accepted_at_milliseconds)
               OR (SELECT COUNT(*) FROM repository_import_facts AS fact
                   WHERE fact.request_id = request.request_id AND fact.fact_kind = 1) != 1
               OR (SELECT fact_sequence FROM repository_import_facts AS fact
                   WHERE fact.request_id = request.request_id AND fact.fact_kind = 1)
                  <= request.accepted_sequence
               OR (request.state = 0 AND (SELECT COUNT(*) FROM repository_import_facts AS fact
                   WHERE fact.request_id = request.request_id AND fact.fact_kind = 2) != 0)
               OR (request.state IN (1, 2, 4) AND
                   (SELECT COUNT(*) FROM repository_import_facts AS fact
                    WHERE fact.request_id = request.request_id AND fact.fact_kind = 2) != 1)
               OR (request.state = 3 AND (SELECT COUNT(*) FROM repository_import_facts AS fact
                   WHERE fact.request_id = request.request_id AND fact.fact_kind = 2) > 1)
               OR EXISTS (
                    SELECT 1 FROM repository_import_facts AS dispatched
                    JOIN repository_import_facts AS prepared
                      ON prepared.request_id = dispatched.request_id AND prepared.fact_kind = 1
                    WHERE dispatched.request_id = request.request_id
                      AND dispatched.fact_kind = 2
                      AND dispatched.fact_sequence <= prepared.fact_sequence
               )
               OR (SELECT COUNT(*) FROM repository_import_facts AS fact
                   WHERE fact.request_id = request.request_id AND fact.fact_kind = 3)
                  != CASE WHEN request.state = 2 THEN 1 ELSE 0 END
               OR (SELECT COUNT(*) FROM repository_import_facts AS fact
                   WHERE fact.request_id = request.request_id AND fact.fact_kind = 4)
                  != CASE WHEN request.state = 3 THEN 1 ELSE 0 END
               OR (SELECT COUNT(*) FROM repository_import_facts AS fact
                   WHERE fact.request_id = request.request_id AND fact.fact_kind = 5)
                  != CASE WHEN request.state = 4 THEN 1 ELSE 0 END
               OR EXISTS (
                    SELECT 1 FROM repository_import_facts AS terminal
                    JOIN repository_import_facts AS prepared
                      ON prepared.request_id = terminal.request_id AND prepared.fact_kind = 1
                    LEFT JOIN repository_import_facts AS dispatched
                      ON dispatched.request_id = terminal.request_id AND dispatched.fact_kind = 2
                    WHERE terminal.request_id = request.request_id
                      AND terminal.fact_kind IN (3, 4, 5)
                      AND terminal.fact_sequence
                          <= COALESCE(dispatched.fact_sequence, prepared.fact_sequence)
               )
               OR EXISTS (
                    SELECT 1 FROM repository_import_facts AS fact
                    WHERE fact.request_id = request.request_id AND (
                        fact.session_id IS NOT request.session_id
                        OR fact.workspace_id IS NOT request.workspace_id
                        OR fact.operation_id IS NOT request.operation_id
                        OR (fact.fact_kind = 3 AND (
                            fact.file_count IS NOT request.file_count
                            OR fact.directory_count IS NOT request.directory_count
                            OR fact.logical_bytes IS NOT request.logical_bytes
                            OR fact.manifest_digest IS NOT request.manifest_digest
                        ))
                    )
               )
               OR EXISTS (
                    SELECT 1 FROM repository_import_audit_facts AS audit
                    WHERE audit.request_id = request.request_id AND (
                        audit.session_id IS NOT request.session_id
                        OR audit.operation_id IS NOT request.operation_id
                    )
               )
               OR (SELECT COUNT(*) FROM repository_import_audit_facts AS audit
                   WHERE audit.request_id = request.request_id AND audit.audit_kind = 1) != 1
               OR (request.state = 0 AND
                   (SELECT COUNT(*) FROM repository_import_audit_facts AS audit
                    WHERE audit.request_id = request.request_id AND audit.audit_kind = 2) != 0)
               OR (request.state IN (1, 2, 4) AND
                   (SELECT COUNT(*) FROM repository_import_audit_facts AS audit
                    WHERE audit.request_id = request.request_id AND audit.audit_kind = 2) != 1)
               OR (request.state = 3 AND
                   (SELECT COUNT(*) FROM repository_import_audit_facts AS audit
                    WHERE audit.request_id = request.request_id AND audit.audit_kind = 2) > 1)
               OR (SELECT COUNT(*) FROM repository_import_audit_facts AS audit
                   WHERE audit.request_id = request.request_id AND audit.audit_kind = 3)
                  != CASE WHEN request.state = 2 THEN 1 ELSE 0 END
               OR (SELECT COUNT(*) FROM repository_import_audit_facts AS audit
                   WHERE audit.request_id = request.request_id AND audit.audit_kind = 4)
                  != CASE WHEN request.state = 3 THEN 1 ELSE 0 END
               OR (SELECT COUNT(*) FROM repository_import_audit_facts AS audit
                   WHERE audit.request_id = request.request_id AND audit.audit_kind = 5)
                  != CASE WHEN request.state = 4 THEN 1 ELSE 0 END
               OR EXISTS (
                    SELECT 1 FROM run_accepted_facts AS run
                    WHERE run.session_id = request.session_id
                      AND run.fact_sequence <= request.accepted_sequence
               )
               OR EXISTS (
                    SELECT 1 FROM session_entries AS entry
                    WHERE entry.session_id = request.session_id
                      AND entry.fact_sequence <= request.accepted_sequence
               )
            UNION ALL
            SELECT 1 FROM mutation_requests AS mutation
            LEFT JOIN repository_import_requests AS request
              ON request.request_id = mutation.request_id
            WHERE mutation.operation_kind = 7 AND request.request_id IS NULL
        )",
        [],
        |row| row.get(0),
    )?;
    if invalid {
        return Err(PersistenceError::InvalidState {
            reason: "repository import facts conflict with durable operation state",
        });
    }
    let mut statement = connection.prepare(
        "SELECT operation_fingerprint, source_path_digest, session_id
         FROM repository_import_requests",
    )?;
    let requests = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, [u8; 32]>(0)?,
                row.get::<_, [u8; 32]>(1)?,
                row.get::<_, [u8; 16]>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (fingerprint, source_path_digest, session_id) in requests {
        if fingerprint
            != import_repository_fingerprint_from_digest(
                SessionId::from_bytes(session_id),
                source_path_digest,
            )
        {
            return Err(PersistenceError::InvalidState {
                reason: "a repository import has an invalid canonical fingerprint",
            });
        }
    }
    Ok(())
}

fn validate_execution_image_facts(connection: &Connection) -> Result<(), PersistenceError> {
    let invalid: bool = connection.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM execution_image_requests AS request
            WHERE (SELECT COUNT(*) FROM execution_image_facts AS fact
                   WHERE fact.request_id = request.request_id AND fact.fact_kind = 1) != 1
               OR (SELECT COUNT(*) FROM execution_image_audit_facts AS audit
                   WHERE audit.request_id = request.request_id AND audit.audit_kind = 1) != 1
               OR (request.state = 0 AND (SELECT COUNT(*) FROM execution_image_facts AS fact
                   WHERE fact.request_id = request.request_id AND fact.fact_kind = 2) != 0)
               OR (request.state IN (1, 2, 4) AND
                   (SELECT COUNT(*) FROM execution_image_facts AS fact
                    WHERE fact.request_id = request.request_id AND fact.fact_kind = 2) != 1)
               OR (request.state = 3 AND (SELECT COUNT(*) FROM execution_image_facts AS fact
                   WHERE fact.request_id = request.request_id AND fact.fact_kind = 2) > 1)
               OR (SELECT COUNT(*) FROM execution_image_facts AS fact
                   WHERE fact.request_id = request.request_id AND fact.fact_kind = 3)
                  != CASE WHEN request.state = 2 THEN 1 ELSE 0 END
               OR (SELECT COUNT(*) FROM execution_image_facts AS fact
                   WHERE fact.request_id = request.request_id AND fact.fact_kind = 4)
                  != CASE WHEN request.state = 3 THEN 1 ELSE 0 END
               OR (SELECT COUNT(*) FROM execution_image_facts AS fact
                   WHERE fact.request_id = request.request_id AND fact.fact_kind = 5)
                  != CASE WHEN request.state = 4 THEN 1 ELSE 0 END
               OR EXISTS (
                    SELECT 1 FROM execution_image_facts AS fact
                    WHERE fact.request_id = request.request_id AND (
                        fact.generation_id IS NOT request.generation_id
                        OR fact.operation_id IS NOT request.operation_id
                        OR (fact.fact_kind = 3 AND (
                            fact.file_count IS NOT request.file_count
                            OR fact.directory_count IS NOT request.directory_count
                            OR fact.logical_bytes IS NOT request.logical_bytes
                            OR fact.manifest_digest IS NOT request.manifest_digest
                        ))
                    )
               )
               OR EXISTS (
                    SELECT 1 FROM execution_image_audit_facts AS audit
                    WHERE audit.request_id = request.request_id AND (
                        audit.generation_id IS NOT request.generation_id
                        OR audit.operation_id IS NOT request.operation_id
                    )
               )
               OR (SELECT COUNT(*) FROM execution_image_audit_facts AS audit
                   WHERE audit.request_id = request.request_id AND audit.audit_kind = 2)
                  != CASE WHEN request.state IN (1, 2, 4) THEN 1
                          WHEN request.state = 3 THEN
                            (SELECT COUNT(*) FROM execution_image_facts AS fact
                             WHERE fact.request_id = request.request_id AND fact.fact_kind = 2)
                          ELSE 0 END
               OR (SELECT COUNT(*) FROM execution_image_audit_facts AS audit
                   WHERE audit.request_id = request.request_id AND audit.audit_kind = 3)
                  != CASE WHEN request.state = 2 THEN 1 ELSE 0 END
               OR (SELECT COUNT(*) FROM execution_image_audit_facts AS audit
                   WHERE audit.request_id = request.request_id AND audit.audit_kind = 4)
                  != CASE WHEN request.state = 3 THEN 1 ELSE 0 END
               OR (SELECT COUNT(*) FROM execution_image_audit_facts AS audit
                   WHERE audit.request_id = request.request_id AND audit.audit_kind = 5)
                  != CASE WHEN request.state = 4 THEN 1 ELSE 0 END
            UNION ALL
            SELECT 1 FROM current_execution_image AS current
            JOIN execution_image_requests AS request ON request.request_id = current.request_id
            WHERE current.singleton IS NOT 1
               OR current.generation_id IS NOT request.generation_id
               OR request.state IS NOT 2
               OR current.updated_sequence IS NOT (
                    SELECT fact_sequence FROM execution_image_facts
                    WHERE request_id = request.request_id AND fact_kind = 3
               )
        )",
        [],
        |row| row.get(0),
    )?;
    if invalid {
        return Err(PersistenceError::InvalidState {
            reason: "execution image facts conflict with durable operation state",
        });
    }
    let mut statement = connection.prepare(
        "SELECT operation_fingerprint, toolchain_source_digest, cargo_source_digest,
                target_os, target_arch, format_version, limits_version
         FROM execution_image_requests",
    )?;
    let requests = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, [u8; 32]>(0)?,
                row.get::<_, [u8; 32]>(1)?,
                row.get::<_, [u8; 32]>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (fingerprint, toolchain, cargo, target_os, target_arch, format, limits) in requests {
        if fingerprint != provision_execution_image_fingerprint(toolchain, cargo)
            || crate::persistence::ExecutionTargetOs::from_record(target_os).is_none()
            || crate::persistence::ExecutionTargetArch::from_record(target_arch).is_none()
            || format != 1
            || limits != 1
        {
            return Err(PersistenceError::InvalidState {
                reason: "an execution image request has invalid canonical input",
            });
        }
    }
    Ok(())
}

fn validate_run_request_payloads(connection: &Connection) -> Result<(), PersistenceError> {
    let mut statement = connection.prepare(
        "SELECT
            request.operation_fingerprint,
            request.session_id,
            request.run_id,
            request.user_message_id,
            accepted.session_id,
            accepted.run_id,
            accepted.user_message_id,
            accepted.open_code_service,
            accepted.model_id,
            accepted.protocol_revision,
            accepted.context_policy_version,
            accepted.source_entry_high_water,
            accepted.estimated_input_tokens,
            accepted.maximum_input_tokens,
            accepted.maximum_output_tokens,
            entry.session_id,
            entry.entry_sequence,
            entry.message_id,
            entry.run_id,
            entry.text
         FROM run_input_requests AS request
         LEFT JOIN run_accepted_facts AS accepted ON accepted.request_id = request.request_id
         LEFT JOIN session_entries AS entry
           ON entry.run_id = request.run_id AND entry.entry_kind = 1",
    )?;
    let inputs = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, [u8; 32]>(0)?,
                row.get::<_, [u8; 16]>(1)?,
                row.get::<_, [u8; 16]>(2)?,
                row.get::<_, [u8; 16]>(3)?,
                row.get::<_, Option<[u8; 16]>>(4)?,
                row.get::<_, Option<[u8; 16]>>(5)?,
                row.get::<_, Option<[u8; 16]>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<i64>>(9)?,
                row.get::<_, Option<i64>>(10)?,
                row.get::<_, Option<i64>>(11)?,
                row.get::<_, Option<i64>>(12)?,
                row.get::<_, Option<i64>>(13)?,
                row.get::<_, Option<i64>>(14)?,
                row.get::<_, Option<[u8; 16]>>(15)?,
                row.get::<_, Option<i64>>(16)?,
                row.get::<_, Option<[u8; 16]>>(17)?,
                row.get::<_, Option<[u8; 16]>>(18)?,
                row.get::<_, Option<String>>(19)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for input in inputs {
        let (
            fingerprint,
            request_session,
            request_run,
            request_message,
            Some(accepted_session),
            Some(accepted_run),
            Some(accepted_message),
            Some(service),
            Some(model_id),
            Some(protocol_revision),
            Some(context_policy_version),
            Some(source_high_water),
            Some(estimated_input_tokens),
            Some(maximum_input_tokens),
            Some(maximum_output_tokens),
            Some(entry_session),
            Some(entry_sequence),
            Some(entry_message),
            Some(entry_run),
            Some(text),
        ) = input
        else {
            return Err(PersistenceError::InvalidState {
                reason: "a run input is missing its accepted run or user message fact",
            });
        };
        let service = run_service_from_record(service)?;
        let selection = RunModelSelection {
            service,
            model_id: model_id.clone(),
            protocol_revision: positive_u16(protocol_revision)?,
            maximum_input_tokens: positive_u32(maximum_input_tokens)?,
            maximum_output_tokens: positive_u32(maximum_output_tokens)?,
            supports_tool_calls: true,
        };
        let session_id = SessionId::from_bytes(request_session);
        if validate_user_text(&text).is_err()
            || validate_model_selection(&selection).is_err()
            || request_session != accepted_session
            || request_session != entry_session
            || request_run != accepted_run
            || request_run != entry_run
            || request_message != accepted_message
            || request_message != entry_message
            || source_high_water != entry_sequence
            || fingerprint
                != submit_session_input_fingerprint(session_id, &text, service, &model_id)
        {
            return Err(PersistenceError::InvalidState {
                reason: "a persisted run input has invalid canonical bindings",
            });
        }
        let (entry_count, text_bytes) = connection.query_row(
            "SELECT COUNT(*), COALESCE(SUM(
                 COALESCE(length(CAST(entry.text AS BLOB)),
                          length(call.input_payload),
                          length(result.result_payload), 0)
             ), 0)
             FROM session_entries AS entry
             LEFT JOIN tool_calls AS call ON call.call_id = entry.tool_call_id
             LEFT JOIN tool_operation_facts AS result
               ON result.call_id = entry.tool_call_id AND result.fact_kind BETWEEN 3 AND 6
             WHERE entry.session_id = ?1 AND entry.entry_sequence <= ?2",
            params![&request_session[..], source_high_water],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?;
        let entry_count = u64::try_from(entry_count).map_err(|_| invalid_run_context())?;
        let text_bytes = u64::try_from(text_bytes).map_err(|_| invalid_run_context())?;
        let estimate = conservative_input_token_estimate(text_bytes, entry_count)
            .ok_or_else(invalid_run_context)?;
        if entry_count == 0
            || entry_count > MAX_CONTEXT_ENTRIES as u64
            || i64::from(estimate) != estimated_input_tokens
            || estimated_input_tokens > maximum_input_tokens
            || context_policy_version != i64::from(CONTEXT_POLICY_VERSION)
        {
            return Err(invalid_run_context());
        }
    }

    let mut statement = connection.prepare(
        "SELECT
            operation_fingerprint,
            session_id,
            run_id,
            result_state,
            result_cancellation_requested,
            intent_applied,
            delivery_event_id
         FROM run_cancellation_requests",
    )?;
    let cancellations = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, [u8; 32]>(0)?,
                row.get::<_, [u8; 16]>(1)?,
                row.get::<_, [u8; 16]>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, bool>(4)?,
                row.get::<_, bool>(5)?,
                row.get::<_, Option<[u8; 16]>>(6)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (fingerprint, session, run, result_state, cancellation_requested, applied, delivery) in
        cancellations
    {
        if fingerprint
            != cancel_run_fingerprint(
                SessionId::from_bytes(session),
                crate::persistence::RunId::from_bytes(run),
            )
            || !(1..=6).contains(&result_state)
            || (applied && (result_state > 2 || !cancellation_requested || delivery.is_none()))
            || (!applied && delivery.is_some())
        {
            return Err(PersistenceError::InvalidState {
                reason: "a persisted run cancellation has invalid canonical input",
            });
        }
    }
    Ok(())
}

fn validate_run_canonical_facts(connection: &Connection) -> Result<(), PersistenceError> {
    let invalid: bool = connection.query_row(
        "SELECT EXISTS (
            SELECT 1
            FROM run_accepted_facts AS accepted
            WHERE EXISTS (
                    SELECT 1 FROM run_input_requests AS request
                    WHERE request.request_id = accepted.request_id
                      AND (request.session_id IS NOT accepted.session_id
                           OR request.run_id IS NOT accepted.run_id
                           OR request.user_message_id IS NOT accepted.user_message_id
                           OR request.accepted_sequence IS NOT accepted.fact_sequence
                           OR request.accepted_at_milliseconds
                              IS NOT accepted.accepted_at_milliseconds)
               )
               OR (SELECT COUNT(*) FROM session_entries AS entry
                   WHERE entry.run_id = accepted.run_id AND entry.entry_kind = 1) != 1
               OR EXISTS (
                    SELECT 1 FROM session_entries AS entry
                    WHERE entry.run_id = accepted.run_id AND entry.entry_kind = 1
                      AND (entry.session_id IS NOT accepted.session_id
                           OR entry.message_id IS NOT accepted.user_message_id
                           OR entry.entry_sequence IS NOT accepted.source_entry_high_water
                           OR entry.fact_sequence >= accepted.fact_sequence)
               )
               OR (SELECT COUNT(*) FROM run_state_facts AS state
                   WHERE state.run_id = accepted.run_id AND state.state BETWEEN 3 AND 7) > 1
               OR EXISTS (
                    SELECT 1 FROM run_state_facts AS state
                    WHERE state.run_id = accepted.run_id
                      AND (state.session_id IS NOT accepted.session_id
                           OR state.fact_sequence <= accepted.fact_sequence)
               )
               OR EXISTS (
                    SELECT 1 FROM run_state_facts AS terminal
                    JOIN run_state_facts AS active ON active.run_id = terminal.run_id
                    WHERE terminal.run_id = accepted.run_id
                      AND terminal.state BETWEEN 3 AND 7
                      AND active.state = 2
                      AND terminal.fact_sequence <= active.fact_sequence
               )
               OR (
                    (SELECT COUNT(*) FROM run_state_facts AS state
                     WHERE state.run_id = accepted.run_id AND state.state = 3) = 1
                  ) != (
                    (SELECT COUNT(*) FROM session_entries AS entry
                     WHERE entry.run_id = accepted.run_id
                       AND entry.entry_kind = 2 AND entry.assistant_phase = 2) = 1
                  )
               OR EXISTS (
                    SELECT 1 FROM session_entries AS entry
                    JOIN run_state_facts AS succeeded ON succeeded.run_id = entry.run_id
                    WHERE entry.run_id = accepted.run_id
                      AND entry.entry_kind = 2
                      AND succeeded.state = 3
                      AND entry.fact_sequence >= succeeded.fact_sequence
               )
            UNION ALL
            SELECT 1
            FROM session_entries AS entry
            JOIN run_accepted_facts AS accepted ON accepted.run_id = entry.run_id
            WHERE entry.entry_kind = 2 AND (
                entry.session_id IS NOT accepted.session_id
                OR entry.open_code_service IS NOT accepted.open_code_service
                OR entry.model_id IS NOT accepted.model_id
            )
            UNION ALL
            SELECT 1
            FROM session_created_facts AS session
            WHERE (SELECT COUNT(*) FROM run_accepted_facts AS accepted
                   WHERE accepted.session_id = session.session_id
                     AND NOT EXISTS (
                         SELECT 1 FROM run_state_facts AS terminal
                         WHERE terminal.run_id = accepted.run_id
                           AND terminal.state BETWEEN 3 AND 7
                     )) > 1
               OR (SELECT COUNT(*) FROM session_entries AS entry
                   WHERE entry.session_id = session.session_id)
                  != COALESCE((SELECT MAX(entry_sequence) FROM session_entries AS entry
                               WHERE entry.session_id = session.session_id), 0)
            UNION ALL
            SELECT 1
            FROM run_cancellation_requests AS cancellation
            JOIN run_accepted_facts AS accepted ON accepted.run_id = cancellation.run_id
            WHERE cancellation.session_id IS NOT accepted.session_id
               OR (cancellation.intent_applied = 1 AND cancellation.result_state NOT IN (1, 2))
            UNION ALL
            SELECT 1
            FROM provider_operation_facts AS fact
            GROUP BY fact.operation_id
            HAVING SUM(fact.fact_kind = 1) != 1
                OR SUM(fact.fact_kind = 2) > 1
                OR SUM(fact.fact_kind IN (3, 4, 5, 6)) > 1
                OR COUNT(DISTINCT hex(fact.run_id)) != 1
                OR MIN(CASE WHEN fact.fact_kind = 1 THEN fact.fact_sequence END)
                   >= COALESCE(MIN(CASE WHEN fact.fact_kind = 2 THEN fact.fact_sequence END),
                               9223372036854775807)
                   AND SUM(fact.fact_kind = 2) = 1
                OR COALESCE(MIN(CASE WHEN fact.fact_kind = 2 THEN fact.fact_sequence END),
                            MIN(CASE WHEN fact.fact_kind = 1 THEN fact.fact_sequence END))
                   >= COALESCE(MIN(CASE WHEN fact.fact_kind IN (3, 4, 5, 6)
                                        THEN fact.fact_sequence END),
                               9223372036854775807)
                   AND SUM(fact.fact_kind IN (3, 4, 5, 6)) = 1
            UNION ALL
            SELECT 1
            FROM provider_operation_facts AS outcome
            WHERE outcome.fact_kind IN (3, 4, 5, 6)
              AND EXISTS (
                  SELECT 1 FROM run_state_facts AS terminal
                  WHERE terminal.run_id = outcome.run_id
                    AND terminal.state BETWEEN 3 AND 7
                    AND outcome.fact_sequence >= terminal.fact_sequence
              )
            UNION ALL
            SELECT 1
            FROM provider_operation_facts AS prepared
            JOIN run_accepted_facts AS accepted ON accepted.run_id = prepared.run_id
            WHERE prepared.fact_kind = 1 AND (
                prepared.open_code_service IS NOT accepted.open_code_service
                OR prepared.model_id IS NOT accepted.model_id
                OR prepared.protocol_revision IS NOT accepted.protocol_revision
                OR prepared.credential_generation IS NOT accepted.credential_generation
                OR prepared.context_policy_version IS NOT accepted.context_policy_version
                OR prepared.tool_catalog_version IS NOT accepted.tool_catalog_version
                OR prepared.tool_limits_version IS NOT accepted.tool_limits_version
                OR prepared.source_entry_high_water < accepted.source_entry_high_water
                OR prepared.estimated_input_tokens IS NULL
                OR NOT EXISTS (
                    SELECT 1 FROM session_entries AS source
                    WHERE source.session_id = accepted.session_id
                      AND source.entry_sequence = prepared.source_entry_high_water
                      AND source.fact_sequence < prepared.fact_sequence
                )
            )
            UNION ALL
            SELECT 1
            FROM provider_operation_facts AS prepared
            WHERE prepared.fact_kind = 1
            GROUP BY prepared.run_id
            HAVING MIN(prepared.turn_index) != 1
                OR MAX(prepared.turn_index) != COUNT(*)
                OR COUNT(DISTINCT prepared.turn_index) != COUNT(*)
            UNION ALL
            SELECT 1
            FROM run_audit_facts AS audit
            WHERE NOT (
                (audit.audit_kind = 1 AND audit.actor_kind = 1
                 AND audit.request_id IS NOT NULL AND audit.operation_id IS NULL
                 AND EXISTS (SELECT 1 FROM run_input_requests AS request
                             WHERE request.request_id = audit.request_id
                               AND request.run_id = audit.run_id))
                OR
                (audit.audit_kind IN (7, 11) AND audit.actor_kind = 1
                 AND audit.request_id IS NOT NULL AND audit.operation_id IS NULL
                 AND EXISTS (SELECT 1 FROM run_cancellation_requests AS request
                             WHERE request.request_id = audit.request_id
                               AND request.run_id = audit.run_id))
                OR
                (audit.audit_kind IN (2, 8, 9, 10) AND audit.actor_kind = 2
                 AND audit.request_id IS NULL AND audit.operation_id IS NULL)
                OR
                (audit.audit_kind IN (3, 4, 5, 6) AND audit.actor_kind = 2
                 AND audit.request_id IS NULL AND audit.operation_id IS NOT NULL
                 AND EXISTS (SELECT 1 FROM provider_operation_facts AS operation
                             WHERE operation.operation_id = audit.operation_id
                               AND operation.run_id = audit.run_id))
            )
            UNION ALL
            SELECT 1
            FROM run_accepted_facts AS accepted
            WHERE (SELECT COUNT(*) FROM run_audit_facts AS audit
                   WHERE audit.run_id = accepted.run_id
                     AND audit.request_id = accepted.request_id
                     AND audit.actor_kind = 1
                     AND audit.audit_kind = 1) != 1
               OR (SELECT COUNT(*) FROM run_audit_facts AS audit
                   WHERE audit.run_id = accepted.run_id AND audit.audit_kind = 2)
                  != (SELECT COUNT(*) FROM run_state_facts AS state
                      WHERE state.run_id = accepted.run_id AND state.state = 2)
               OR (SELECT COUNT(*) FROM run_audit_facts AS audit
                   WHERE audit.run_id = accepted.run_id AND audit.audit_kind = 8)
                  != (SELECT COUNT(*) FROM run_state_facts AS state
                      WHERE state.run_id = accepted.run_id AND state.state = 3)
               OR (SELECT COUNT(*) FROM run_audit_facts AS audit
                   WHERE audit.run_id = accepted.run_id AND audit.audit_kind = 9)
                  != (SELECT COUNT(*) FROM run_state_facts AS state
                      WHERE state.run_id = accepted.run_id AND state.state = 4)
               OR (SELECT COUNT(*) FROM run_audit_facts AS audit
                   WHERE audit.run_id = accepted.run_id AND audit.audit_kind = 10)
                  != (SELECT COUNT(*) FROM run_state_facts AS state
                      WHERE state.run_id = accepted.run_id AND state.state IN (5, 6, 7))
            UNION ALL
            SELECT 1
            FROM run_cancellation_requests AS cancellation
            WHERE (SELECT COUNT(*) FROM run_audit_facts AS audit
                   WHERE audit.request_id = cancellation.request_id
                     AND audit.run_id = cancellation.run_id
                     AND audit.actor_kind = 1
                     AND audit.audit_kind = CASE cancellation.intent_applied
                                               WHEN 1 THEN 7 ELSE 11 END) != 1
            UNION ALL
            SELECT 1
            FROM provider_operation_facts AS prepared
            WHERE prepared.fact_kind = 1 AND (
                (SELECT COUNT(*) FROM run_audit_facts AS audit
                 WHERE audit.operation_id = prepared.operation_id
                   AND audit.audit_kind = 3) != 1
                OR (SELECT COUNT(*) FROM run_audit_facts AS audit
                    WHERE audit.operation_id = prepared.operation_id
                      AND audit.audit_kind = 4)
                   != (SELECT COUNT(*) FROM provider_operation_facts AS fact
                       WHERE fact.operation_id = prepared.operation_id AND fact.fact_kind = 2)
                OR (SELECT COUNT(*) FROM run_audit_facts AS audit
                    WHERE audit.operation_id = prepared.operation_id
                      AND audit.audit_kind = 5)
                   != (SELECT COUNT(*) FROM provider_operation_facts AS fact
                       WHERE fact.operation_id = prepared.operation_id AND fact.fact_kind = 3)
                OR (SELECT COUNT(*) FROM run_audit_facts AS audit
                    WHERE audit.operation_id = prepared.operation_id
                      AND audit.audit_kind = 6)
                   != (SELECT COUNT(*) FROM provider_operation_facts AS fact
                       WHERE fact.operation_id = prepared.operation_id
                         AND fact.fact_kind IN (4, 5, 6))
            )
        )",
        [],
        |row| row.get(0),
    )?;
    if invalid {
        return Err(PersistenceError::InvalidState {
            reason: "canonical run facts have invalid ordering or provenance",
        });
    }
    Ok(())
}

fn validate_tool_facts(connection: &Connection) -> Result<(), PersistenceError> {
    let invalid: bool = connection.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM run_accepted_facts AS accepted
            WHERE NOT (
                (accepted.tool_catalog_version = 0 AND accepted.tool_limits_version = 0)
                OR
                (accepted.tool_catalog_version = 1 AND accepted.tool_limits_version = 1
                 AND EXISTS (
                     SELECT 1 FROM repository_import_facts AS repository
                     WHERE repository.session_id = accepted.session_id
                       AND repository.fact_kind = 3
                       AND repository.fact_sequence < accepted.fact_sequence
                 ))
            )
            UNION ALL
            SELECT 1 FROM tool_calls AS call
            JOIN run_accepted_facts AS run ON run.run_id = call.run_id
            WHERE call.session_id IS NOT run.session_id
               OR call.fact_sequence <= run.fact_sequence
               OR (SELECT COUNT(*) FROM provider_operation_facts AS provider
                   WHERE provider.operation_id = call.provider_operation_id
                     AND provider.run_id = call.run_id AND provider.fact_kind = 3) != 1
               OR call.fact_sequence <= (SELECT provider.fact_sequence
                   FROM provider_operation_facts AS provider
                   WHERE provider.operation_id = call.provider_operation_id
                     AND provider.fact_kind = 3)
               OR (SELECT COUNT(*) FROM session_entries AS entry
                   WHERE entry.tool_call_id = call.call_id AND entry.entry_kind = 3
                     AND entry.run_id = call.run_id AND entry.session_id = call.session_id) != 1
               OR call.fact_sequence >= (SELECT entry.fact_sequence FROM session_entries AS entry
                   WHERE entry.tool_call_id = call.call_id AND entry.entry_kind = 3)
               OR (SELECT COUNT(*) FROM tool_audit_facts AS audit
                   WHERE audit.call_id = call.call_id AND audit.audit_kind = 1) != 1
               OR (SELECT COUNT(*) FROM tool_audit_facts AS audit
                   WHERE audit.call_id = call.call_id AND audit.audit_kind = 2)
                  != (SELECT COUNT(*) FROM tool_operation_facts AS fact
                      WHERE fact.call_id = call.call_id AND fact.fact_kind = 1)
               OR (SELECT COUNT(*) FROM tool_audit_facts AS audit
                   WHERE audit.call_id = call.call_id AND audit.audit_kind = 3)
                  != (SELECT COUNT(*) FROM tool_operation_facts AS fact
                      WHERE fact.call_id = call.call_id AND fact.fact_kind = 2)
               OR (SELECT COUNT(*) FROM tool_audit_facts AS audit
                   WHERE audit.call_id = call.call_id AND audit.audit_kind = 4)
                  != (SELECT COUNT(*) FROM tool_operation_facts AS fact
                      WHERE fact.call_id = call.call_id AND fact.fact_kind BETWEEN 3 AND 5)
               OR (SELECT COUNT(*) FROM tool_audit_facts AS audit
                   WHERE audit.call_id = call.call_id AND audit.audit_kind = 5)
                  != (SELECT COUNT(*) FROM tool_operation_facts AS fact
                      WHERE fact.call_id = call.call_id AND fact.fact_kind = 6)
            UNION ALL
            SELECT 1 FROM tool_operation_facts AS fact
            JOIN tool_calls AS call ON call.call_id = fact.call_id
            WHERE fact.operation_id IS NOT call.operation_id
               OR fact.session_id IS NOT call.session_id
               OR fact.run_id IS NOT call.run_id
               OR fact.fact_sequence <= call.fact_sequence
            UNION ALL
            SELECT 1 FROM tool_calls AS call
            WHERE (SELECT COUNT(*) FROM tool_operation_facts AS prepared
                   WHERE prepared.call_id = call.call_id AND prepared.fact_kind = 1) > 1
               OR (SELECT COUNT(*) FROM tool_operation_facts AS dispatched
                   WHERE dispatched.call_id = call.call_id AND dispatched.fact_kind = 2)
                  > (SELECT COUNT(*) FROM tool_operation_facts AS prepared
                     WHERE prepared.call_id = call.call_id AND prepared.fact_kind = 1)
               OR EXISTS (
                    SELECT 1 FROM tool_operation_facts AS dispatched
                    JOIN tool_operation_facts AS prepared ON prepared.call_id = dispatched.call_id
                    WHERE dispatched.call_id = call.call_id AND dispatched.fact_kind = 2
                      AND prepared.fact_kind = 1
                      AND dispatched.fact_sequence <= prepared.fact_sequence
               )
               OR EXISTS (
                    SELECT 1 FROM tool_operation_facts AS terminal
                    JOIN tool_operation_facts AS prepared ON prepared.call_id = terminal.call_id
                    LEFT JOIN tool_operation_facts AS dispatched
                      ON dispatched.call_id = terminal.call_id AND dispatched.fact_kind = 2
                    WHERE terminal.call_id = call.call_id
                      AND terminal.fact_kind BETWEEN 3 AND 6
                      AND prepared.fact_kind = 1
                      AND terminal.fact_sequence <= COALESCE(
                          dispatched.fact_sequence, prepared.fact_sequence
                      )
               )
            UNION ALL
            SELECT 1 FROM tool_operation_facts AS terminal
            JOIN tool_calls AS call ON call.call_id = terminal.call_id
            WHERE terminal.fact_kind BETWEEN 3 AND 6 AND (
                (SELECT COUNT(*) FROM session_entries AS entry
                 WHERE entry.tool_call_id = terminal.call_id AND entry.entry_kind = 4
                   AND entry.run_id = terminal.run_id
                   AND entry.session_id = terminal.session_id) != 1
                OR terminal.fact_sequence >= (SELECT entry.fact_sequence
                    FROM session_entries AS entry
                    WHERE entry.tool_call_id = terminal.call_id AND entry.entry_kind = 4)
                OR (terminal.fact_kind = 6) != (terminal.workspace_delivery_event_id IS NOT NULL)
                OR (terminal.fact_kind = 6 AND NOT EXISTS (
                    SELECT 1 FROM run_state_facts AS state
                    WHERE state.run_id = terminal.run_id AND state.state = 7
                      AND state.fact_sequence > terminal.fact_sequence
                ))
            )
            UNION ALL
            SELECT 1 FROM tool_uncertainty_acknowledgements AS acknowledgement
            WHERE NOT EXISTS (
                    SELECT 1 FROM tool_operation_facts AS uncertain
                    WHERE uncertain.run_id = acknowledgement.run_id
                      AND uncertain.session_id = acknowledgement.session_id
                      AND uncertain.fact_kind = 6
                      AND uncertain.fact_sequence < acknowledgement.fact_sequence
               )
               OR (SELECT COUNT(*) FROM tool_audit_facts AS audit
                   WHERE audit.request_id = acknowledgement.request_id
                     AND audit.audit_kind = 6
                     AND audit.run_id = acknowledgement.run_id
                     AND audit.session_id = acknowledgement.session_id) != 1
            UNION ALL
            SELECT 1 FROM tool_audit_facts AS audit
            WHERE NOT (
                (audit.audit_kind BETWEEN 1 AND 5
                 AND EXISTS (SELECT 1 FROM tool_calls AS call
                             WHERE call.call_id = audit.call_id
                               AND call.operation_id = audit.operation_id
                               AND call.run_id = audit.run_id
                               AND call.session_id = audit.session_id
                               AND call.tool_kind = audit.tool_kind
                               AND call.path_digest = audit.path_digest))
                OR
                (audit.audit_kind = 6
                 AND EXISTS (SELECT 1 FROM tool_uncertainty_acknowledgements AS acknowledgement
                             WHERE acknowledgement.request_id = audit.request_id
                               AND acknowledgement.run_id = audit.run_id
                               AND acknowledgement.session_id = audit.session_id))
            )
        )",
        [],
        |row| row.get(0),
    )?;
    if invalid {
        return Err(PersistenceError::InvalidState {
            reason: "canonical tool facts have invalid ordering or provenance",
        });
    }

    let mut statement = connection
        .prepare("SELECT call_id, tool_kind, input_payload, path_digest FROM tool_calls")?;
    let calls = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, [u8; 16]>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, [u8; 32]>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (_, tool_kind, payload, path_digest) in calls {
        let input: ToolInput =
            serde_json::from_slice(&payload).map_err(|_| PersistenceError::InvalidState {
                reason: "a canonical tool call has an invalid typed input payload",
            })?;
        if !validate_canonical_input(&input)
            || input.kind().to_record() != tool_kind
            || tool_path_digest(input.path().as_str()) != path_digest
        {
            return Err(PersistenceError::InvalidState {
                reason: "a canonical tool call has invalid typed input",
            });
        }
    }

    let mut statement = connection.prepare(
        "SELECT call.tool_kind, prepared.recovery_payload,
                EXISTS (SELECT 1 FROM tool_operation_facts AS dispatched
                        WHERE dispatched.call_id = prepared.call_id AND dispatched.fact_kind = 2)
         FROM tool_operation_facts AS prepared
         JOIN tool_calls AS call ON call.call_id = prepared.call_id
         WHERE prepared.fact_kind = 1",
    )?;
    let plans = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<Vec<u8>>>(1)?,
                row.get::<_, bool>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (tool_kind, plan, dispatched) in plans {
        let tool_kind = ToolKind::from_record(tool_kind).ok_or(PersistenceError::InvalidState {
            reason: "a prepared tool operation has an invalid tool kind",
        })?;
        let valid = if tool_kind.is_mutation() {
            plan.as_deref().is_some_and(recovery_plan_is_valid) || !dispatched && plan.is_none()
        } else {
            plan.is_none()
        };
        if !valid {
            return Err(PersistenceError::InvalidState {
                reason: "a prepared tool operation has an invalid recovery plan",
            });
        }
    }

    let mut statement = connection.prepare(
        "SELECT terminal.fact_kind, terminal.result_status, terminal.result_payload, call.tool_kind
         FROM tool_operation_facts AS terminal
         JOIN tool_calls AS call ON call.call_id = terminal.call_id
         WHERE terminal.fact_kind BETWEEN 3 AND 6",
    )?;
    let results = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (fact_kind, result_status, payload, tool_kind) in results {
        let result: ToolResult =
            serde_json::from_slice(&payload).map_err(|_| PersistenceError::InvalidState {
                reason: "a canonical tool result has an invalid typed payload",
            })?;
        let tool_kind = ToolKind::from_record(tool_kind).ok_or(PersistenceError::InvalidState {
            reason: "a canonical tool result has an invalid tool kind",
        })?;
        let valid = validate_canonical_result(tool_kind, &result)
            && match result {
                ToolResult::Ok { .. } => fact_kind == 3 && result_status == 1,
                ToolResult::Error {
                    error: ToolErrorKind::Uncertain,
                } => fact_kind == 6 && result_status == 4,
                ToolResult::Error {
                    error:
                        ToolErrorKind::Interrupted
                        | ToolErrorKind::Cancelled
                        | ToolErrorKind::NotDispatched,
                } => matches!(fact_kind, 4 | 5) && result_status == 3,
                ToolResult::Error { .. } => fact_kind == 3 && result_status == 2,
            };
        if !valid {
            return Err(PersistenceError::InvalidState {
                reason: "a canonical tool result conflicts with its terminal fact",
            });
        }
    }

    let mut statement = connection.prepare(
        "SELECT operation_fingerprint, session_id, run_id
         FROM tool_uncertainty_acknowledgements",
    )?;
    let acknowledgements = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, [u8; 32]>(0)?,
                row.get::<_, [u8; 16]>(1)?,
                row.get::<_, [u8; 16]>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (fingerprint, session_id, run_id) in acknowledgements {
        if fingerprint
            != acknowledge_tool_uncertainty_fingerprint(
                SessionId::from_bytes(session_id),
                crate::persistence::RunId::from_bytes(run_id),
            )
        {
            return Err(PersistenceError::InvalidState {
                reason: "a tool uncertainty acknowledgement has invalid canonical input",
            });
        }
    }
    Ok(())
}

fn validate_logical_sequences(connection: &Connection) -> Result<(), PersistenceError> {
    let invalid: bool = connection.query_row(
        "WITH canonical_sequences(sequence) AS (
            SELECT accepted_sequence FROM session_creation_requests
            UNION ALL SELECT fact_sequence FROM workspace_operation_facts
            UNION ALL SELECT fact_sequence FROM session_created_facts
            UNION ALL SELECT audit_sequence FROM audit_facts
            UNION ALL SELECT accepted_sequence FROM credential_mutation_requests
            UNION ALL SELECT fact_sequence FROM credential_operation_facts
            UNION ALL SELECT audit_sequence FROM credential_audit_facts
            UNION ALL SELECT accepted_sequence FROM server_stop_requests
            UNION ALL SELECT audit_sequence FROM server_audit_facts
            UNION ALL SELECT fact_sequence FROM session_entries
            UNION ALL SELECT fact_sequence FROM run_accepted_facts
            UNION ALL SELECT fact_sequence FROM run_state_facts
            UNION ALL SELECT fact_sequence FROM run_cancellation_requests
            UNION ALL SELECT fact_sequence FROM provider_operation_facts
            UNION ALL SELECT audit_sequence FROM run_audit_facts
         )
         SELECT EXISTS (
            SELECT 1 FROM canonical_sequences
            GROUP BY sequence HAVING COUNT(*) != 1
            UNION ALL
            SELECT 1 FROM logical_sequences
            WHERE singleton != 1
               OR next_value <= COALESCE((SELECT MAX(sequence) FROM canonical_sequences), 0)
         )",
        [],
        |row| row.get(0),
    )?;
    if invalid {
        return Err(PersistenceError::InvalidState {
            reason: "canonical logical sequences are invalid",
        });
    }
    validate_repository_logical_sequences(connection)?;
    validate_tool_logical_sequences(connection)
}

fn validate_repository_logical_sequences(connection: &Connection) -> Result<(), PersistenceError> {
    let mut statement = connection.prepare(
        "SELECT accepted_sequence FROM repository_import_requests
         UNION ALL SELECT fact_sequence FROM repository_import_facts
         UNION ALL SELECT audit_sequence FROM repository_import_audit_facts",
    )?;
    let sequences = statement
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut unique = std::collections::BTreeSet::new();
    for sequence in &sequences {
        if *sequence <= 0 || !unique.insert(*sequence) {
            return Err(invalid_repository_sequences());
        }
        let collides: bool = connection.query_row(
            "SELECT
                EXISTS (SELECT 1 FROM session_creation_requests WHERE accepted_sequence = ?1)
             OR EXISTS (SELECT 1 FROM workspace_operation_facts WHERE fact_sequence = ?1)
             OR EXISTS (SELECT 1 FROM session_created_facts WHERE fact_sequence = ?1)
             OR EXISTS (SELECT 1 FROM audit_facts WHERE audit_sequence = ?1)
             OR EXISTS (SELECT 1 FROM credential_mutation_requests WHERE accepted_sequence = ?1)
             OR EXISTS (SELECT 1 FROM credential_operation_facts WHERE fact_sequence = ?1)
             OR EXISTS (SELECT 1 FROM credential_audit_facts WHERE audit_sequence = ?1)
             OR EXISTS (SELECT 1 FROM server_stop_requests WHERE accepted_sequence = ?1)
             OR EXISTS (SELECT 1 FROM server_audit_facts WHERE audit_sequence = ?1)
             OR EXISTS (SELECT 1 FROM session_entries WHERE fact_sequence = ?1)
             OR EXISTS (SELECT 1 FROM run_accepted_facts WHERE fact_sequence = ?1)
             OR EXISTS (SELECT 1 FROM run_state_facts WHERE fact_sequence = ?1)
             OR EXISTS (SELECT 1 FROM run_cancellation_requests WHERE fact_sequence = ?1)
             OR EXISTS (SELECT 1 FROM provider_operation_facts WHERE fact_sequence = ?1)
             OR EXISTS (SELECT 1 FROM run_audit_facts WHERE audit_sequence = ?1)
             OR EXISTS (SELECT 1 FROM tool_calls WHERE fact_sequence = ?1)
             OR EXISTS (SELECT 1 FROM tool_operation_facts WHERE fact_sequence = ?1)
             OR EXISTS (SELECT 1 FROM tool_uncertainty_acknowledgements WHERE fact_sequence = ?1)
             OR EXISTS (SELECT 1 FROM tool_audit_facts WHERE audit_sequence = ?1)",
            [sequence],
            |row| row.get(0),
        )?;
        if collides {
            return Err(invalid_repository_sequences());
        }
    }
    let next_value: i64 = connection.query_row(
        "SELECT next_value FROM logical_sequences WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    if sequences
        .into_iter()
        .max()
        .is_some_and(|max| next_value <= max)
    {
        return Err(invalid_repository_sequences());
    }
    Ok(())
}

fn validate_tool_logical_sequences(connection: &Connection) -> Result<(), PersistenceError> {
    let mut statement = connection.prepare(
        "SELECT fact_sequence FROM tool_calls
         UNION ALL SELECT fact_sequence FROM tool_operation_facts
         UNION ALL SELECT fact_sequence FROM tool_uncertainty_acknowledgements
         UNION ALL SELECT audit_sequence FROM tool_audit_facts",
    )?;
    let sequences = statement
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut unique = std::collections::BTreeSet::new();
    for sequence in &sequences {
        if *sequence <= 0 || !unique.insert(*sequence) {
            return Err(invalid_tool_sequences());
        }
        let collides: bool = connection.query_row(
            "SELECT
                EXISTS (SELECT 1 FROM session_creation_requests WHERE accepted_sequence = ?1)
             OR EXISTS (SELECT 1 FROM workspace_operation_facts WHERE fact_sequence = ?1)
             OR EXISTS (SELECT 1 FROM session_created_facts WHERE fact_sequence = ?1)
             OR EXISTS (SELECT 1 FROM audit_facts WHERE audit_sequence = ?1)
             OR EXISTS (SELECT 1 FROM credential_mutation_requests WHERE accepted_sequence = ?1)
             OR EXISTS (SELECT 1 FROM credential_operation_facts WHERE fact_sequence = ?1)
             OR EXISTS (SELECT 1 FROM credential_audit_facts WHERE audit_sequence = ?1)
             OR EXISTS (SELECT 1 FROM server_stop_requests WHERE accepted_sequence = ?1)
             OR EXISTS (SELECT 1 FROM server_audit_facts WHERE audit_sequence = ?1)
             OR EXISTS (SELECT 1 FROM session_entries WHERE fact_sequence = ?1)
             OR EXISTS (SELECT 1 FROM run_accepted_facts WHERE fact_sequence = ?1)
             OR EXISTS (SELECT 1 FROM run_state_facts WHERE fact_sequence = ?1)
             OR EXISTS (SELECT 1 FROM run_cancellation_requests WHERE fact_sequence = ?1)
             OR EXISTS (SELECT 1 FROM provider_operation_facts WHERE fact_sequence = ?1)
             OR EXISTS (SELECT 1 FROM run_audit_facts WHERE audit_sequence = ?1)
             OR EXISTS (SELECT 1 FROM repository_import_requests WHERE accepted_sequence = ?1)
             OR EXISTS (SELECT 1 FROM repository_import_facts WHERE fact_sequence = ?1)
             OR EXISTS (SELECT 1 FROM repository_import_audit_facts WHERE audit_sequence = ?1)",
            [sequence],
            |row| row.get(0),
        )?;
        if collides {
            return Err(invalid_tool_sequences());
        }
    }
    let next_value: i64 = connection.query_row(
        "SELECT next_value FROM logical_sequences WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    if sequences
        .into_iter()
        .max()
        .is_some_and(|max| next_value <= max)
    {
        return Err(invalid_tool_sequences());
    }
    Ok(())
}

const fn invalid_tool_sequences() -> PersistenceError {
    PersistenceError::InvalidState {
        reason: "tool operation logical sequences are invalid",
    }
}

const fn invalid_repository_sequences() -> PersistenceError {
    PersistenceError::InvalidState {
        reason: "repository import logical sequences are invalid",
    }
}

fn run_service_from_record(value: i64) -> Result<RunOpenCodeService, PersistenceError> {
    match value {
        1 => Ok(RunOpenCodeService::Zen),
        2 => Ok(RunOpenCodeService::Go),
        _ => Err(PersistenceError::InvalidState {
            reason: "a persisted run service is invalid",
        }),
    }
}

fn positive_u16(value: i64) -> Result<u16, PersistenceError> {
    u16::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or(PersistenceError::InvalidState {
            reason: "a persisted run protocol revision is invalid",
        })
}

fn positive_u32(value: i64) -> Result<u32, PersistenceError> {
    u32::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or(PersistenceError::InvalidState {
            reason: "a persisted run token limit is invalid",
        })
}

const fn invalid_run_context() -> PersistenceError {
    PersistenceError::InvalidState {
        reason: "a persisted run context binding is invalid",
    }
}
