use rusqlite::Connection;

use crate::persistence::PersistenceError;

pub(super) fn rebuild(connection: &mut Connection) -> Result<(), PersistenceError> {
    let transaction = connection.transaction()?;
    transaction.execute("DELETE FROM session_run_states", [])?;
    transaction.execute("DELETE FROM runs", [])?;
    transaction.execute("DELETE FROM sessions", [])?;
    transaction.execute("DELETE FROM delivery_events", [])?;

    transaction.execute(
        "WITH canonical_updates(session_id, sequence) AS (
            SELECT session_id, fact_sequence FROM session_created_facts
            UNION ALL SELECT session_id, fact_sequence FROM run_accepted_facts
            UNION ALL SELECT session_id, fact_sequence FROM session_entries
            UNION ALL SELECT session_id, fact_sequence FROM run_state_facts
            UNION ALL SELECT session_id, fact_sequence
                      FROM run_cancellation_requests WHERE intent_applied = 1
            UNION ALL SELECT session_id, fact_sequence
                      FROM repository_import_facts WHERE fact_kind IN (1, 3, 4, 5)
            UNION ALL SELECT session_id, fact_sequence FROM tool_calls
            UNION ALL SELECT session_id, fact_sequence FROM tool_operation_facts
            UNION ALL SELECT session_id, fact_sequence FROM tool_uncertainty_acknowledgements
            UNION ALL SELECT session_id, updated_sequence FROM local_commands
            UNION ALL SELECT session_id, accepted_sequence FROM local_command_cancellations
            UNION ALL SELECT session_id, accepted_sequence FROM session_rename_requests
            UNION ALL SELECT session_id, accepted_sequence FROM session_archive_requests WHERE state = 2
         ), latest_updates AS (
            SELECT session_id, MAX(sequence) AS updated_sequence
            FROM canonical_updates GROUP BY session_id
         )
         INSERT INTO sessions (
            session_id, workspace_id, display_name, working_directory, created_sequence,
            updated_sequence, created_at_milliseconds, lifecycle, archived
         )
         SELECT
            fact.session_id,
            fact.workspace_id,
            COALESCE((
                SELECT rename.display_name FROM session_rename_requests AS rename
                WHERE rename.session_id = fact.session_id
                ORDER BY rename.accepted_sequence DESC LIMIT 1
            ), fact.display_name),
            fact.working_directory,
            fact.accepted_sequence,
            updates.updated_sequence,
            fact.created_at_milliseconds,
            1,
            COALESCE((
                SELECT archive.archived FROM session_archive_requests AS archive
                WHERE archive.session_id = fact.session_id AND archive.state = 2
                ORDER BY archive.accepted_sequence DESC LIMIT 1
            ), 0)
         FROM session_created_facts AS fact
         JOIN latest_updates AS updates ON updates.session_id = fact.session_id",
        [],
    )?;

    transaction.execute(
        "INSERT INTO runs (
            run_id, session_id, user_message_id, open_code_service, model_id,
            protocol_revision, credential_generation, context_policy_version,
            tool_catalog_version, tool_limits_version, execution_image_generation,
            source_entry_high_water, estimated_input_tokens,
            maximum_input_tokens, maximum_output_tokens, provider_turns, tool_calls,
            tool_mutations, tool_result_bytes, state, cancellation_requested, failure_kind,
            accepted_sequence, updated_sequence, accepted_at_milliseconds,
            updated_at_milliseconds
         )
         SELECT
            accepted.run_id,
            accepted.session_id,
            accepted.user_message_id,
            accepted.open_code_service,
            accepted.model_id,
            accepted.protocol_revision,
            accepted.credential_generation,
            accepted.context_policy_version,
            accepted.tool_catalog_version,
            accepted.tool_limits_version,
            accepted.execution_image_generation,
            accepted.source_entry_high_water,
            accepted.estimated_input_tokens,
            accepted.maximum_input_tokens,
            accepted.maximum_output_tokens,
            (SELECT COUNT(*) FROM provider_operation_facts AS provider
             WHERE provider.run_id = accepted.run_id AND provider.fact_kind = 1),
            (SELECT COUNT(*) FROM tool_calls AS call
             WHERE call.run_id = accepted.run_id),
            (SELECT COUNT(*) FROM tool_calls AS call
             WHERE call.run_id = accepted.run_id AND call.tool_kind BETWEEN 4 AND 7),
            COALESCE((SELECT SUM(length(result.result_payload))
                      FROM tool_operation_facts AS result
                      WHERE result.run_id = accepted.run_id
                        AND result.fact_kind BETWEEN 3 AND 6), 0),
            COALESCE((SELECT state.state FROM run_state_facts AS state
                      WHERE state.run_id = accepted.run_id
                      ORDER BY state.fact_sequence DESC LIMIT 1), 1),
            EXISTS (SELECT 1 FROM run_cancellation_requests AS cancellation
                    WHERE cancellation.run_id = accepted.run_id
                      AND cancellation.intent_applied = 1),
            (SELECT state.failure_kind FROM run_state_facts AS state
             WHERE state.run_id = accepted.run_id
             ORDER BY state.fact_sequence DESC LIMIT 1),
            accepted.fact_sequence,
            MAX(
                accepted.fact_sequence,
                COALESCE((SELECT MAX(state.fact_sequence) FROM run_state_facts AS state
                          WHERE state.run_id = accepted.run_id), 0),
                COALESCE((SELECT MAX(cancellation.fact_sequence)
                          FROM run_cancellation_requests AS cancellation
                          WHERE cancellation.run_id = accepted.run_id
                            AND cancellation.intent_applied = 1), 0),
                COALESCE((SELECT MAX(call.fact_sequence) FROM tool_calls AS call
                          WHERE call.run_id = accepted.run_id), 0),
                COALESCE((SELECT MAX(tool.fact_sequence) FROM tool_operation_facts AS tool
                          WHERE tool.run_id = accepted.run_id), 0)
            ),
            accepted.accepted_at_milliseconds,
            COALESCE((
                SELECT update_time FROM (
                    SELECT state.fact_sequence AS sequence,
                           state.created_at_milliseconds AS update_time
                    FROM run_state_facts AS state
                    WHERE state.run_id = accepted.run_id
                    UNION ALL
                    SELECT cancellation.fact_sequence,
                           cancellation.accepted_at_milliseconds
                    FROM run_cancellation_requests AS cancellation
                    WHERE cancellation.run_id = accepted.run_id
                      AND cancellation.intent_applied = 1
                    UNION ALL
                    SELECT call.fact_sequence, call.created_at_milliseconds
                    FROM tool_calls AS call WHERE call.run_id = accepted.run_id
                    UNION ALL
                    SELECT tool.fact_sequence, tool.created_at_milliseconds
                    FROM tool_operation_facts AS tool WHERE tool.run_id = accepted.run_id
                ) ORDER BY sequence DESC LIMIT 1
            ), accepted.accepted_at_milliseconds)
         FROM run_accepted_facts AS accepted",
        [],
    )?;

    transaction.execute(
        "WITH canonical_updates(session_id, sequence) AS (
            SELECT session_id, fact_sequence FROM session_created_facts
            UNION ALL SELECT session_id, fact_sequence FROM run_accepted_facts
            UNION ALL SELECT session_id, fact_sequence FROM session_entries
            UNION ALL SELECT session_id, fact_sequence FROM run_state_facts
            UNION ALL SELECT session_id, fact_sequence
                      FROM run_cancellation_requests WHERE intent_applied = 1
            UNION ALL SELECT session_id, fact_sequence
                      FROM repository_import_facts WHERE fact_kind IN (1, 3, 4, 5)
            UNION ALL SELECT session_id, fact_sequence FROM tool_calls
            UNION ALL SELECT session_id, fact_sequence FROM tool_operation_facts
            UNION ALL SELECT session_id, fact_sequence FROM tool_uncertainty_acknowledgements
            UNION ALL SELECT session_id, updated_sequence FROM local_commands
            UNION ALL SELECT session_id, accepted_sequence FROM local_command_cancellations
            UNION ALL SELECT session_id, accepted_sequence FROM session_rename_requests
            UNION ALL SELECT session_id, accepted_sequence FROM session_archive_requests WHERE state = 2
         ), latest_updates AS (
            SELECT session_id, MAX(sequence) AS updated_sequence
            FROM canonical_updates GROUP BY session_id
         )
         INSERT INTO session_run_states (
            session_id, active_run_id, entry_high_water, updated_sequence
         )
         SELECT
            session.session_id,
            (SELECT run.run_id FROM runs AS run
             WHERE run.session_id = session.session_id AND run.state IN (1, 2)
             LIMIT 1),
            MAX(
                COALESCE((SELECT MAX(entry.entry_sequence) FROM session_entries AS entry
                          WHERE entry.session_id = session.session_id), 0),
                COALESCE((SELECT MAX(command.entry_sequence) FROM local_commands AS command
                          WHERE command.session_id = session.session_id), 0)
            ),
            updates.updated_sequence
         FROM session_created_facts AS session
         JOIN latest_updates AS updates ON updates.session_id = session.session_id",
        [],
    )?;

    transaction.execute(
        "INSERT INTO delivery_events (
            event_id, event_sequence, session_id, event_kind,
            payload_version, created_at_milliseconds
         )
         SELECT delivery_event_id, fact_sequence, session_id, 1, 1,
                created_at_milliseconds
         FROM session_created_facts
         UNION ALL
         SELECT delivery_event_id, accepted_sequence, session_id, 18, 1,
                accepted_at_milliseconds
         FROM session_rename_requests
         UNION ALL
         SELECT delivery_event_id, accepted_sequence, session_id, 19, 1,
                accepted_at_milliseconds
         FROM session_archive_requests WHERE state = 2
         UNION ALL
         SELECT delivery_event_id, fact_sequence, session_id,
                CASE entry_kind WHEN 1 THEN 2 WHEN 2 THEN 6 WHEN 3 THEN 12 ELSE 13 END,
                1, created_at_milliseconds
         FROM session_entries
         UNION ALL
         SELECT accepted_event_id, accepted_sequence, session_id, 16, 1,
                accepted_at_milliseconds
         FROM local_commands
         UNION ALL
         SELECT delivery_event_id, updated_sequence, session_id, 17, 1,
                updated_at_milliseconds
         FROM local_commands WHERE state BETWEEN 3 AND 5
         UNION ALL
         SELECT delivery_event_id, fact_sequence, session_id, 3, 1,
                accepted_at_milliseconds
         FROM run_accepted_facts
         UNION ALL
         SELECT delivery_event_id, fact_sequence, session_id,
                CASE state
                    WHEN 2 THEN 4
                    WHEN 3 THEN 7
                    WHEN 4 THEN 8
                    WHEN 5 THEN 9
                    WHEN 6 THEN 10
                    WHEN 7 THEN 14
                END,
                1, created_at_milliseconds
         FROM run_state_facts
         UNION ALL
         SELECT delivery_event_id, fact_sequence, session_id, 5, 1,
                accepted_at_milliseconds
         FROM run_cancellation_requests
         WHERE intent_applied = 1
         UNION ALL
         SELECT delivery_event_id, fact_sequence, session_id, 11, 1,
                created_at_milliseconds
         FROM repository_import_facts
         WHERE fact_kind IN (1, 3, 4, 5)
         UNION ALL
         SELECT workspace_delivery_event_id, fact_sequence, session_id, 15, 1,
                created_at_milliseconds
         FROM tool_operation_facts
         WHERE fact_kind = 6
         UNION ALL
         SELECT delivery_event_id, fact_sequence, session_id, 15, 1,
                accepted_at_milliseconds
         FROM tool_uncertainty_acknowledgements",
        [],
    )?;
    transaction.commit()?;
    Ok(())
}
