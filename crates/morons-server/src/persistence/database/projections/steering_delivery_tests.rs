use super::validate;
use rusqlite::Connection;

fn delivery_connection() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    // Minimal canonical tables isolate eligibility from schema and projection validation.
    db.execute_batch(
        "CREATE TABLE session_entries (
            message_id INTEGER, session_id INTEGER, run_id INTEGER, entry_kind INTEGER,
            actor_kind INTEGER, created_at_milliseconds INTEGER, entry_sequence INTEGER,
            fact_sequence INTEGER, text TEXT, tool_call_id INTEGER
         );
         CREATE TABLE run_accepted_facts (
            run_id INTEGER, user_message_id INTEGER, fact_sequence INTEGER
         );
         CREATE TABLE steering_delivery_facts (
            message_id INTEGER, session_id INTEGER, run_id INTEGER,
            created_at_milliseconds INTEGER, source_entry_high_water INTEGER,
            fact_sequence INTEGER, item_id INTEGER, enqueue_sequence INTEGER, item_revision INTEGER
         );
         CREATE TABLE steering_mutation_requests (
            session_id INTEGER, accepted_sequence INTEGER, target_run_id INTEGER,
            change_kind INTEGER, queue_revision INTEGER, item_id INTEGER,
            item_revision INTEGER, text TEXT
         );
         CREATE TABLE steering_lifecycle_facts (
            session_id INTEGER, fact_sequence INTEGER, target_run_id INTEGER
         );
         CREATE TABLE run_state_facts (run_id INTEGER, state INTEGER, fact_sequence INTEGER);
         CREATE TABLE run_cancellation_requests (
            run_id INTEGER, intent_applied INTEGER, fact_sequence INTEGER
         );
         CREATE TABLE session_archive_requests (
            session_id INTEGER, archived INTEGER, accepted_sequence INTEGER
         );
         CREATE TABLE provider_operation_facts (
            run_id INTEGER, operation_id INTEGER, fact_kind INTEGER, fact_sequence INTEGER
         );
         CREATE TABLE tool_calls (run_id INTEGER, call_id INTEGER, fact_sequence INTEGER);
         INSERT INTO session_entries VALUES
            (100, 1, 10, 1, 1, 0, 1, 1, 'Initial', NULL),
            (101, 1, 10, 1, 1, 100, 2, 20, 'Edited', NULL);
         INSERT INTO run_accepted_facts VALUES (10, 100, 2);
         INSERT INTO run_state_facts VALUES (10, 2, 3);
         INSERT INTO steering_mutation_requests VALUES
            (1, 4, 10, 1, 1, 200, 1, 'Queued'),
            (1, 5, NULL, 2, 2, 200, 2, 'Edited'),
            (1, 6, 10, 5, 3, NULL, NULL, NULL);
         INSERT INTO steering_delivery_facts VALUES (101, 1, 10, 100, 1, 21, 200, 4, 2);",
    )
    .unwrap();
    validate(&db).unwrap();
    db
}

#[test]
fn steering_delivery_preserves_fifo_across_multiple_deliveries() {
    let db = delivery_connection();
    db.execute_batch(
        "INSERT INTO steering_mutation_requests VALUES (1, 7, NULL, 1, 4, 201, 1, 'Second');
         INSERT INTO session_entries VALUES (102, 1, 10, 1, 1, 101, 3, 22, 'Second', NULL);
         INSERT INTO steering_delivery_facts VALUES (102, 1, 10, 101, 2, 23, 201, 7, 1);",
    )
    .unwrap();
    validate(&db).unwrap();
    for corruption in [
        "DELETE FROM steering_delivery_facts WHERE item_id = 200",
        "UPDATE steering_delivery_facts SET source_entry_high_water = 1 WHERE item_id = 201",
        // Swap item provenance and text without changing either committed entry boundary.
        "UPDATE steering_delivery_facts SET
             item_id = CASE message_id WHEN 101 THEN 201 ELSE 200 END,
             enqueue_sequence = CASE message_id WHEN 101 THEN 7 ELSE 4 END,
             item_revision = CASE message_id WHEN 101 THEN 1 ELSE 2 END;
         UPDATE session_entries SET text = CASE message_id WHEN 101 THEN 'Second' ELSE 'Edited' END
             WHERE message_id IN (101, 102)",
    ] {
        db.execute_batch("SAVEPOINT deliveries").unwrap();
        db.execute_batch(corruption).unwrap();
        assert!(
            validate(&db).is_err(),
            "accepted invalid delivery order or provenance: {corruption}"
        );
        db.execute_batch("ROLLBACK TO deliveries; RELEASE deliveries")
            .unwrap();
    }
    validate(&db).unwrap();
}

#[test]
fn steering_delivery_rejects_unsafe_boundaries() {
    let db = delivery_connection();
    for corruption in [
        "DELETE FROM run_state_facts",
        "INSERT INTO run_state_facts VALUES (10, 3, 19)",
        "INSERT INTO run_cancellation_requests VALUES (10, 1, 19)",
        "INSERT INTO session_archive_requests VALUES (1, 1, 19)",
        "INSERT INTO steering_lifecycle_facts VALUES (1, 19, 10)",
        "UPDATE steering_mutation_requests SET target_run_id = 11 WHERE change_kind = 5",
        "UPDATE steering_delivery_facts SET item_revision = 1",
        "UPDATE session_entries SET text = 'Queued' WHERE message_id = 101",
        "INSERT INTO steering_mutation_requests VALUES (1, 22, NULL, 2, 5, 200, 3, 'Later')",
        "INSERT INTO steering_mutation_requests VALUES (1, 22, NULL, 3, 5, 200, 3, NULL)",
        "INSERT INTO steering_mutation_requests VALUES (1, 3, 10, 1, 1, 199, 1, 'Earlier')",
        "INSERT INTO provider_operation_facts VALUES (10, 300, 1, 10)",
        "INSERT INTO provider_operation_facts VALUES (10, 300, 1, 10), (10, 300, 2, 11)",
        "INSERT INTO provider_operation_facts VALUES (10, 300, 1, 10), (10, 300, 3, 22)",
        "INSERT INTO tool_calls VALUES (10, 400, 10)",
        "INSERT INTO tool_calls VALUES (10, 400, 10);
         INSERT INTO session_entries VALUES (102, 1, 10, 4, 3, 100, 3, 22, 'Result', 400)",
    ] {
        db.execute_batch("SAVEPOINT boundary").unwrap();
        db.execute_batch(corruption).unwrap();
        assert!(
            validate(&db).is_err(),
            "accepted unsafe boundary: {corruption}"
        );
        db.execute_batch("ROLLBACK TO boundary; RELEASE boundary")
            .unwrap();
    }
}

#[test]
fn steering_delivery_accepts_resolved_work_and_later_lifecycle_changes() {
    let db = delivery_connection();
    for history in [
        "INSERT INTO run_state_facts VALUES (10, 3, 22)",
        "INSERT INTO run_cancellation_requests VALUES (10, 1, 22)",
        "INSERT INTO run_cancellation_requests VALUES (10, 0, 19)",
        "INSERT INTO session_archive_requests VALUES (1, 1, 22)",
        "INSERT INTO session_archive_requests VALUES (1, 1, 10), (1, 0, 11)",
        "INSERT INTO steering_lifecycle_facts VALUES (1, 22, 10)",
        "INSERT INTO steering_mutation_requests VALUES
            (1, 3, 10, 1, 1, 199, 1, 'Earlier'), (1, 7, NULL, 3, 4, 199, 2, NULL)",
        "INSERT INTO provider_operation_facts VALUES (10, 300, 1, 10), (10, 300, 3, 11)",
        "INSERT INTO provider_operation_facts VALUES (10, 300, 1, 10), (10, 300, 4, 11)",
        "INSERT INTO provider_operation_facts VALUES (10, 300, 1, 10), (10, 300, 5, 11)",
        "INSERT INTO provider_operation_facts VALUES (10, 300, 1, 10), (10, 300, 6, 11)",
        "INSERT INTO tool_calls VALUES (10, 400, 10);
         INSERT INTO session_entries VALUES (102, 1, 10, 4, 3, 100, 2, 11, 'Result', 400);
         UPDATE session_entries SET entry_sequence = 3 WHERE message_id = 101;
         UPDATE steering_delivery_facts SET source_entry_high_water = 2",
    ] {
        db.execute_batch("SAVEPOINT boundary").unwrap();
        db.execute_batch(history).unwrap();
        validate(&db).unwrap_or_else(|error| panic!("rejected safe boundary {history}: {error}"));
        db.execute_batch("ROLLBACK TO boundary; RELEASE boundary")
            .unwrap();
    }
}
