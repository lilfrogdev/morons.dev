use rusqlite::Connection;

use crate::persistence::PersistenceError;

const HISTORY: &str = "WITH history AS (
            SELECT session_id, queue_revision, accepted_sequence AS sequence,
                CASE WHEN change_kind = 5 THEN 0
                     WHEN change_kind = 4 OR queue_revision = 1 THEN 1 END AS paused,
                target_run_id
            FROM steering_mutation_requests
            UNION ALL
            SELECT session_id, queue_revision, fact_sequence, 1, target_run_id
            FROM steering_lifecycle_facts
            UNION ALL
            SELECT session_id, queue_revision, fact_sequence, NULL, NULL
            FROM steering_delivery_facts
        )";

pub(super) fn validate(connection: &Connection) -> Result<(), PersistenceError> {
    validate_history(connection)?;
    validate_projection(connection)
}

pub(super) fn validate_history(connection: &Connection) -> Result<(), PersistenceError> {
    let invalid: bool = connection.query_row(
        &format!("{HISTORY}, sources AS (
            SELECT session_id, run_id, fact_sequence AS sequence, 1 AS reason
            FROM run_cancellation_requests WHERE intent_applied = 1
            UNION ALL
            SELECT session_id, run_id, fact_sequence, 2
            FROM run_state_facts WHERE state BETWEEN 3 AND 7
            UNION ALL
            SELECT session_id, NULL, accepted_sequence, 3
            FROM session_archive_requests WHERE archived = 1
        ), ordered AS (
            SELECT *, LAG(queue_revision) OVER queue_order AS previous_revision,
                LAG(sequence) OVER queue_order AS previous_sequence
            FROM history
            WINDOW queue_order AS (PARTITION BY session_id ORDER BY sequence)
        )
        SELECT EXISTS (
            SELECT 1 FROM steering_mutation_requests AS request
            WHERE (request.queue_revision = 1 AND request.change_kind != 1)
               OR (request.change_kind IN (1, 5) AND (
                NOT EXISTS (SELECT 1 FROM run_accepted_facts AS run
                    WHERE run.session_id = request.session_id AND run.run_id = request.target_run_id
                      AND run.fact_sequence < request.accepted_sequence)
                OR EXISTS (SELECT 1 FROM sources AS source
                    WHERE source.session_id = request.session_id
                      AND source.run_id = request.target_run_id
                      AND source.reason IN (1, 2) AND source.sequence < request.accepted_sequence)
            ))
               OR (SELECT archived FROM session_archive_requests AS archive
                    WHERE archive.session_id = request.session_id
                      AND archive.accepted_sequence < request.accepted_sequence
                    ORDER BY archive.accepted_sequence DESC LIMIT 1) = 1
               OR (request.change_kind = 1 AND request.target_run_id IS NOT (
                    SELECT prior.target_run_id FROM history AS prior
                    WHERE prior.session_id = request.session_id AND prior.target_run_id IS NOT NULL
                      AND prior.sequence < request.accepted_sequence
                    ORDER BY prior.sequence DESC LIMIT 1)
                   AND EXISTS (SELECT 1 FROM history AS prior
                    WHERE prior.session_id = request.session_id AND prior.target_run_id IS NOT NULL
                      AND prior.sequence < request.accepted_sequence))
            UNION ALL
            SELECT 1 FROM sources AS source
            WHERE (SELECT prior.paused FROM history AS prior
                    WHERE prior.session_id = source.session_id AND prior.paused IS NOT NULL
                      AND prior.sequence < source.sequence ORDER BY prior.sequence DESC LIMIT 1) = 0
              AND (source.run_id IS NULL OR source.run_id = (
                    SELECT prior.target_run_id FROM history AS prior
                    WHERE prior.session_id = source.session_id AND prior.target_run_id IS NOT NULL
                      AND prior.sequence < source.sequence ORDER BY prior.sequence DESC LIMIT 1))
              AND NOT EXISTS (SELECT 1 FROM steering_lifecycle_facts AS fact
                    WHERE fact.session_id = source.session_id AND fact.reason = source.reason
                      AND fact.source_sequence = source.sequence)
            UNION ALL
            SELECT 1 FROM ordered
            WHERE previous_revision IS NOT NULL
              AND (queue_revision != previous_revision + 1 OR sequence <= previous_sequence)
            UNION ALL
            SELECT 1 FROM history GROUP BY session_id, queue_revision HAVING COUNT(*) != 1
            UNION ALL
            SELECT 1 FROM steering_lifecycle_facts AS fact
            WHERE EXISTS (SELECT 1 FROM history AS prior
                    WHERE prior.session_id = fact.session_id AND prior.paused = 1
                      AND prior.sequence < fact.fact_sequence
                      AND NOT EXISTS (SELECT 1 FROM history AS newer
                        WHERE newer.session_id = prior.session_id AND newer.paused IS NOT NULL
                          AND newer.sequence > prior.sequence AND newer.sequence < fact.fact_sequence))
               OR (fact.source_sequence IS NOT NULL AND fact.source_sequence >= fact.fact_sequence)
               OR (fact.reason = 1 AND NOT EXISTS (
                    SELECT 1 FROM run_cancellation_requests AS source
                    WHERE source.fact_sequence = fact.source_sequence
                      AND source.session_id = fact.session_id AND source.run_id = fact.target_run_id
                      AND source.intent_applied = 1
                      AND source.accepted_at_milliseconds = fact.created_at_milliseconds))
               OR (fact.reason = 2 AND NOT EXISTS (
                    SELECT 1 FROM run_state_facts AS source
                    WHERE source.fact_sequence = fact.source_sequence
                      AND source.session_id = fact.session_id AND source.run_id = fact.target_run_id
                      AND source.state BETWEEN 3 AND 7
                      AND source.created_at_milliseconds = fact.created_at_milliseconds))
               OR (fact.reason = 3 AND NOT EXISTS (
                    SELECT 1 FROM session_archive_requests AS source
                    WHERE source.accepted_sequence = fact.source_sequence
                      AND source.session_id = fact.session_id AND source.archived = 1
                      AND source.accepted_at_milliseconds = fact.created_at_milliseconds))
               OR EXISTS (SELECT 1 FROM history AS target
                    WHERE target.session_id = fact.session_id AND target.sequence < fact.fact_sequence
                      AND target.target_run_id IS NOT NULL
                      AND target.target_run_id != fact.target_run_id
                      AND NOT EXISTS (SELECT 1 FROM history AS newer
                        WHERE newer.session_id = target.session_id AND newer.target_run_id IS NOT NULL
                          AND newer.sequence > target.sequence AND newer.sequence < fact.fact_sequence))
        )"),
        [],
        |row| row.get(0),
    )?;
    if invalid {
        return Err(PersistenceError::InvalidState {
            reason: "steering lifecycle history conflicts with its source or queue",
        });
    }
    Ok(())
}

fn validate_projection(connection: &Connection) -> Result<(), PersistenceError> {
    let invalid: bool = connection.query_row(
        &format!(
            "{HISTORY}
        SELECT EXISTS (
            SELECT 1 FROM history AS latest
            LEFT JOIN steering_queues AS queue USING (session_id)
            WHERE NOT EXISTS (SELECT 1 FROM history AS newer
                WHERE newer.session_id = latest.session_id AND newer.sequence > latest.sequence)
              AND queue.revision IS NOT latest.queue_revision
            UNION ALL
            SELECT 1 FROM history AS state
            JOIN steering_queues AS queue USING (session_id)
            WHERE state.paused IS NOT NULL AND queue.paused IS NOT state.paused
              AND NOT EXISTS (SELECT 1 FROM history AS newer
                WHERE newer.session_id = state.session_id AND newer.paused IS NOT NULL
                  AND newer.sequence > state.sequence)
            UNION ALL
            SELECT 1 FROM history AS target
            JOIN steering_queues AS queue USING (session_id)
            WHERE target.target_run_id IS NOT NULL
              AND queue.target_run_id IS NOT target.target_run_id
              AND NOT EXISTS (SELECT 1 FROM history AS newer
                WHERE newer.session_id = target.session_id AND newer.target_run_id IS NOT NULL
                  AND newer.sequence > target.sequence)
        )"
        ),
        [],
        |row| row.get(0),
    )?;
    if invalid {
        return Err(PersistenceError::InvalidState {
            reason: "steering lifecycle history conflicts with its source or queue",
        });
    }
    Ok(())
}
