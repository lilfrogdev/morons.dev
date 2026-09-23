#[cfg(test)]
#[path = "steering_delivery_tests.rs"]
mod tests;

use rusqlite::Connection;

use crate::persistence::PersistenceError;

pub(super) fn validate(connection: &Connection) -> Result<(), PersistenceError> {
    let invalid: bool = connection.query_row(
        "WITH history AS (
            SELECT session_id, accepted_sequence AS sequence, target_run_id,
                   CASE WHEN change_kind = 5 THEN 0
                        WHEN change_kind = 4 OR queue_revision = 1 THEN 1 END AS paused
            FROM steering_mutation_requests
            UNION ALL
            SELECT session_id, fact_sequence, target_run_id, 1 FROM steering_lifecycle_facts
        )
        SELECT EXISTS (
            SELECT 1 FROM session_entries AS entry
            JOIN run_accepted_facts AS run USING (run_id)
            WHERE entry.entry_kind = 1 AND entry.message_id != run.user_message_id
              AND NOT EXISTS (SELECT 1 FROM steering_delivery_facts AS delivery
                  WHERE delivery.message_id = entry.message_id)
            UNION ALL
            SELECT 1 FROM steering_delivery_facts AS delivery
            LEFT JOIN session_entries AS entry USING (message_id)
            LEFT JOIN run_accepted_facts AS run ON run.run_id = delivery.run_id
            WHERE entry.message_id IS NULL OR run.run_id IS NULL
               OR entry.session_id IS NOT delivery.session_id
               OR entry.run_id IS NOT delivery.run_id
               OR entry.entry_kind IS NOT 1 OR entry.actor_kind IS NOT 1
               OR entry.message_id = run.user_message_id
               OR entry.created_at_milliseconds IS NOT delivery.created_at_milliseconds
               OR entry.entry_sequence != delivery.source_entry_high_water + 1
               OR entry.fact_sequence <= run.fact_sequence
               OR entry.fact_sequence >= delivery.fact_sequence
               OR NOT EXISTS (SELECT 1 FROM session_entries AS source
                   WHERE source.session_id = delivery.session_id
                     AND source.entry_sequence = delivery.source_entry_high_water
                     AND source.fact_sequence < entry.fact_sequence)
               OR NOT EXISTS (SELECT 1 FROM run_state_facts AS active
                   WHERE active.run_id = delivery.run_id AND active.state = 2
                     AND active.fact_sequence < entry.fact_sequence)
               OR EXISTS (SELECT 1 FROM run_state_facts AS terminal
                   WHERE terminal.run_id = delivery.run_id AND terminal.state BETWEEN 3 AND 7
                     AND terminal.fact_sequence < delivery.fact_sequence)
               OR EXISTS (SELECT 1 FROM run_cancellation_requests AS cancellation
                   WHERE cancellation.run_id = delivery.run_id AND cancellation.intent_applied = 1
                     AND cancellation.fact_sequence < delivery.fact_sequence)
               OR (SELECT archived FROM session_archive_requests AS archive
                   WHERE archive.session_id = delivery.session_id
                     AND archive.accepted_sequence < delivery.fact_sequence
                   ORDER BY archive.accepted_sequence DESC LIMIT 1) = 1
               OR (SELECT paused FROM history WHERE session_id = delivery.session_id
                   AND paused IS NOT NULL AND sequence < delivery.fact_sequence
                   ORDER BY sequence DESC LIMIT 1) IS NOT 0
               OR (SELECT target_run_id FROM history WHERE session_id = delivery.session_id
                   AND target_run_id IS NOT NULL AND sequence < delivery.fact_sequence
                   ORDER BY sequence DESC LIMIT 1) IS NOT delivery.run_id
               OR NOT EXISTS (SELECT 1 FROM steering_mutation_requests AS enqueue
                   WHERE enqueue.session_id = delivery.session_id AND enqueue.item_id = delivery.item_id
                     AND enqueue.change_kind = 1 AND enqueue.accepted_sequence = delivery.enqueue_sequence
                     AND enqueue.accepted_sequence < entry.fact_sequence)
               OR NOT EXISTS (SELECT 1 FROM steering_mutation_requests AS latest
                   WHERE latest.session_id = delivery.session_id AND latest.item_id = delivery.item_id
                     AND latest.change_kind IN (1, 2) AND latest.item_revision = delivery.item_revision
                     AND latest.text = entry.text AND latest.accepted_sequence < entry.fact_sequence
                     AND NOT EXISTS (SELECT 1 FROM steering_mutation_requests AS newer
                         WHERE newer.item_id = latest.item_id
                           AND newer.accepted_sequence > latest.accepted_sequence))
               OR EXISTS (SELECT 1 FROM steering_mutation_requests AS earlier
                   WHERE earlier.session_id = delivery.session_id AND earlier.change_kind = 1
                     AND earlier.accepted_sequence < delivery.enqueue_sequence
                     AND NOT EXISTS (SELECT 1 FROM steering_mutation_requests AS removed
                         WHERE removed.item_id = earlier.item_id AND removed.change_kind = 3
                           AND removed.accepted_sequence < delivery.fact_sequence)
                     AND NOT EXISTS (SELECT 1 FROM steering_delivery_facts AS consumed
                         WHERE consumed.item_id = earlier.item_id
                           AND consumed.fact_sequence < delivery.fact_sequence))
               OR EXISTS (SELECT 1 FROM provider_operation_facts AS uncertain
                   WHERE uncertain.run_id = delivery.run_id AND uncertain.fact_kind = 5
                     AND uncertain.fact_sequence < delivery.fact_sequence)
               OR EXISTS (SELECT 1 FROM provider_operation_facts AS prepared
                   WHERE prepared.run_id = delivery.run_id AND prepared.fact_kind = 1
                     AND prepared.fact_sequence < delivery.fact_sequence
                     AND NOT EXISTS (SELECT 1 FROM provider_operation_facts AS outcome
                         WHERE outcome.operation_id = prepared.operation_id
                           AND outcome.fact_kind IN (3, 4, 5, 6)
                           AND outcome.fact_sequence < entry.fact_sequence))
               OR EXISTS (SELECT 1 FROM tool_calls AS call
                   WHERE call.run_id = delivery.run_id AND call.fact_sequence < delivery.fact_sequence
                     AND NOT EXISTS (SELECT 1 FROM session_entries AS result
                         WHERE result.tool_call_id = call.call_id AND result.entry_kind = 4
                           AND result.fact_sequence < entry.fact_sequence))
        )",
        [],
        |row| row.get(0),
    )?;
    if invalid {
        return Err(PersistenceError::InvalidState {
            reason: "steering delivery has invalid canonical provenance or boundary",
        });
    }
    Ok(())
}
