use rusqlite::Connection;

use crate::persistence::PersistenceError;

// Foundation-only queues have no canonical history and must remain untouched.
pub(super) fn rebuild(connection: &Connection) -> Result<(), PersistenceError> {
    connection.execute_batch(
        "DELETE FROM steering_pending_messages
         WHERE session_id IN (
             SELECT session_id FROM steering_mutation_requests WHERE queue_revision = 1
         );
         DELETE FROM steering_queues
         WHERE session_id IN (
             SELECT session_id FROM steering_mutation_requests WHERE queue_revision = 1
         );
         WITH history AS (
             SELECT session_id, queue_revision, accepted_sequence AS sequence,
                    target_run_id,
                    CASE WHEN change_kind = 5 THEN 0
                         WHEN change_kind = 4 OR queue_revision = 1 THEN 1 END AS paused
             FROM steering_mutation_requests
             UNION ALL
             SELECT session_id, queue_revision, fact_sequence, target_run_id, 1
             FROM steering_lifecycle_facts
         )
         INSERT INTO steering_queues (session_id, target_run_id, revision, paused)
         SELECT initial.session_id,
                (SELECT target_run_id FROM history
                 WHERE session_id = initial.session_id AND target_run_id IS NOT NULL
                 ORDER BY sequence DESC LIMIT 1),
                (SELECT MAX(queue_revision) FROM history
                 WHERE session_id = initial.session_id),
                (SELECT paused FROM history
                 WHERE session_id = initial.session_id AND paused IS NOT NULL
                 ORDER BY sequence DESC LIMIT 1)
         FROM steering_mutation_requests AS initial WHERE queue_revision = 1;
         WITH pending AS (
             SELECT latest.*, enqueue.accepted_sequence AS enqueue_sequence,
                    enqueue.accepted_at_milliseconds AS created_at_milliseconds
             FROM steering_mutation_requests AS latest
             JOIN steering_mutation_requests AS enqueue
               ON enqueue.session_id = latest.session_id AND enqueue.item_id = latest.item_id
              AND enqueue.change_kind = 1
             WHERE latest.change_kind IN (1, 2)
               AND EXISTS (SELECT 1 FROM steering_mutation_requests AS initial
                   WHERE initial.session_id = latest.session_id AND initial.queue_revision = 1)
               AND NOT EXISTS (SELECT 1 FROM steering_mutation_requests AS newer
                   WHERE newer.session_id = latest.session_id AND newer.item_id = latest.item_id
                     AND newer.accepted_sequence > latest.accepted_sequence)
         )
         INSERT INTO steering_pending_messages (
             item_id, session_id, slot, enqueue_sequence, revision, text, actor,
             created_at_milliseconds
         )
         SELECT item_id, session_id,
                ROW_NUMBER() OVER (PARTITION BY session_id ORDER BY enqueue_sequence),
                enqueue_sequence, item_revision, text, actor, created_at_milliseconds
         FROM pending;",
    )?;
    Ok(())
}
