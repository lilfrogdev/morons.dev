mod rebuild;

use rusqlite::{Connection, params};

use super::validate_integrity;
use crate::persistence::{
    PersistenceError, RunModelSelection, RunOpenCodeService, SessionId,
    run_types::{CONTEXT_POLICY_VERSION, MAX_CONTEXT_ENTRIES, conservative_input_token_estimate},
    types::{
        cancel_run_fingerprint, create_session_fingerprint, stop_server_fingerprint,
        submit_session_input_fingerprint, validate_display_name, validate_model_selection,
        validate_user_text,
    },
};

pub(super) fn repair(connection: &mut Connection) -> Result<(), PersistenceError> {
    validate_session_creation_facts(connection)?;
    validate_mutation_registry(connection)?;
    validate_run_request_payloads(connection)?;
    validate_server_stop_facts(connection)?;
    validate_run_canonical_facts(connection)?;
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
            "SELECT COUNT(*), COALESCE(SUM(length(CAST(text AS BLOB))), 0)
             FROM session_entries
             WHERE session_id = ?1 AND entry_sequence <= ?2",
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
                   WHERE state.run_id = accepted.run_id AND state.state BETWEEN 3 AND 6) > 1
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
                      AND terminal.state BETWEEN 3 AND 6
                      AND active.state = 2
                      AND terminal.fact_sequence <= active.fact_sequence
               )
               OR (
                    (SELECT COUNT(*) FROM run_state_facts AS state
                     WHERE state.run_id = accepted.run_id AND state.state = 3) = 1
                  ) != (
                    (SELECT COUNT(*) FROM session_entries AS entry
                     WHERE entry.run_id = accepted.run_id AND entry.entry_kind = 2) = 1
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
                           AND terminal.state BETWEEN 3 AND 6
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
            WHERE outcome.fact_kind IN (3, 4, 5, 6) AND (
                (SELECT COUNT(*) FROM run_state_facts AS terminal
                 WHERE terminal.run_id = outcome.run_id AND terminal.state BETWEEN 3 AND 6) != 1
                OR outcome.fact_sequence >= (
                    SELECT terminal.fact_sequence FROM run_state_facts AS terminal
                    WHERE terminal.run_id = outcome.run_id AND terminal.state BETWEEN 3 AND 6
                )
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
                OR prepared.source_entry_high_water IS NOT accepted.source_entry_high_water
            )
            UNION ALL
            SELECT 1
            FROM provider_operation_facts AS fact
            GROUP BY fact.run_id
            HAVING COUNT(DISTINCT hex(fact.operation_id)) > 1
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
    Ok(())
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
