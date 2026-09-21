use super::{steering_rebuild, steering_revision, validate_steering_capacity_history};
use rusqlite::{Connection, params};

#[tokio::test(flavor = "current_thread")]
async fn steering_full_rebuild_rolls_back_execution_and_postvalidation_failures() {
    use crate::persistence::{
        MutationRequestId, RunModelSelection, RunService, SessionStore,
        steering::{SteeringChange, SteeringMutation},
        tests::TestRoot,
    };
    let root = TestRoot::new("steering-rebuild-rollback");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    store
        .set_open_code_credential(
            MutationRequestId::from_bytes([1; 16]),
            0,
            b"not-a-real-test-key".to_vec(),
        )
        .await
        .unwrap();
    let session = store
        .create_session(MutationRequestId::from_bytes([2; 16]), None)
        .await
        .unwrap();
    let run = store
        .accept_session_input(
            MutationRequestId::from_bytes([3; 16]),
            session.id,
            "Initial".into(),
            RunModelSelection {
                service: RunService::Zen,
                model_id: "muse-spark-1.2".into(),
                protocol_revision: 1,
                maximum_input_tokens: 96_000,
                maximum_output_tokens: 32_000,
                supports_tool_calls: true,
                supports_image_input: false,
            },
        )
        .await
        .unwrap()
        .run;
    store
        .mutate_steering(SteeringMutation {
            request_id: MutationRequestId::from_bytes([4; 16]),
            session_id: session.id,
            expected_revision: 0,
            change: SteeringChange::Enqueue {
                run_id: run.id,
                text: "Canonical".into(),
            },
        })
        .await
        .unwrap();
    drop(store);
    let mut db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    db.execute("UPDATE steering_pending_messages SET text = 'Damaged'", [])
        .unwrap();
    for body in [
        "SELECT RAISE(ABORT, 'injected rebuild failure');",
        "UPDATE steering_pending_messages SET text = 'Invalid rebuilt text';",
    ] {
        // Invoke the real repair pipeline after schema admission to inject rebuild failures.
        db.execute_batch(&format!(
            "CREATE TEMP TRIGGER inject_failure AFTER INSERT ON sessions BEGIN {body} END;"
        ))
        .unwrap();
        assert!(super::repair(&mut db).is_err());
        assert!(db.is_autocommit());
        assert_eq!(
            db.query_row("SELECT text FROM steering_pending_messages", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
            "Damaged"
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        db.execute_batch("DROP TRIGGER inject_failure").unwrap();
    }
    super::repair(&mut db).unwrap();
    assert_eq!(
        db.query_row("SELECT text FROM steering_pending_messages", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap(),
        "Canonical"
    );
}

#[test]
fn steering_lifecycle_history_validation_is_independent_of_projections() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE steering_mutation_requests (
                session_id INTEGER, queue_revision INTEGER, accepted_sequence INTEGER,
                change_kind INTEGER, target_run_id INTEGER
             );
             CREATE TABLE steering_lifecycle_facts (
                session_id INTEGER, queue_revision INTEGER, fact_sequence INTEGER,
                target_run_id INTEGER, reason INTEGER, source_sequence INTEGER,
                created_at_milliseconds INTEGER
             );
             CREATE TABLE run_accepted_facts (
                session_id INTEGER, run_id INTEGER, fact_sequence INTEGER
             );
             CREATE TABLE run_cancellation_requests (
                session_id INTEGER, run_id INTEGER, fact_sequence INTEGER,
                intent_applied INTEGER, accepted_at_milliseconds INTEGER
             );
             CREATE TABLE run_state_facts (
                session_id INTEGER, run_id INTEGER, fact_sequence INTEGER,
                state INTEGER, created_at_milliseconds INTEGER
             );
             CREATE TABLE session_archive_requests (
                session_id INTEGER, accepted_sequence INTEGER, archived INTEGER,
                accepted_at_milliseconds INTEGER
             );
             INSERT INTO run_accepted_facts VALUES (1, 10, 1);
             INSERT INTO steering_mutation_requests VALUES
                (1, 1, 2, 1, 10), (1, 2, 3, 5, 10);
             INSERT INTO run_state_facts VALUES (1, 10, 4, 3, 100);
             INSERT INTO steering_lifecycle_facts VALUES (1, 3, 5, 10, 2, 4, 100);",
        )
        .unwrap();
    super::steering_lifecycle::validate_history(&connection).unwrap();
    for corruption in [
        "DELETE FROM steering_lifecycle_facts",
        "UPDATE steering_lifecycle_facts SET target_run_id = 11",
        "UPDATE steering_lifecycle_facts SET queue_revision = 4",
        "UPDATE steering_lifecycle_facts SET created_at_milliseconds = 101",
        "UPDATE steering_mutation_requests SET accepted_sequence = 6 WHERE change_kind = 5",
    ] {
        connection.execute_batch("SAVEPOINT corruption").unwrap();
        connection.execute_batch(corruption).unwrap();
        assert!(
            super::steering_lifecycle::validate_history(&connection).is_err(),
            "accepted invalid canonical history: {corruption}"
        );
        connection
            .execute_batch("ROLLBACK TO corruption; RELEASE corruption")
            .unwrap();
    }
    connection
        .execute_batch(
            "CREATE TABLE steering_queues (
                session_id INTEGER, revision INTEGER, paused INTEGER, target_run_id INTEGER
             );",
        )
        .unwrap();
    assert!(super::steering_lifecycle::validate(&connection).is_err());
    connection
        .execute_batch("INSERT INTO steering_queues VALUES (1, 3, 1, 10)")
        .unwrap();
    super::steering_lifecycle::validate(&connection).unwrap();
}

#[test]
fn steering_mutation_history_validation_is_independent_of_projections() {
    use crate::persistence::{
        RunId, SessionId,
        steering::{SteeringChange, fingerprint},
    };

    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE mutation_requests (
            request_id INTEGER, operation_kind INTEGER, accepted_sequence INTEGER,
            accepted_at_milliseconds INTEGER
         );
         CREATE TABLE steering_mutation_requests (
            request_id INTEGER, session_id BLOB, queue_revision INTEGER,
            accepted_sequence INTEGER, accepted_at_milliseconds INTEGER,
            change_kind INTEGER, target_run_id BLOB, item_id BLOB,
            item_revision INTEGER, text TEXT, actor INTEGER, operation_fingerprint BLOB
         );
         INSERT INTO mutation_requests VALUES (1, 18, 2, 100);",
        )
        .unwrap();
    let session = SessionId::from_bytes([1; 16]);
    let run = RunId::from_bytes([2; 16]);
    let change = SteeringChange::Enqueue {
        run_id: run,
        text: "queued".into(),
    };
    connection.execute(
        "INSERT INTO steering_mutation_requests VALUES (1, ?1, 1, 2, 100, 1, ?2, ?3, 1, 'queued', 1, ?4)",
        params![[1_u8; 16], [2_u8; 16], [3_u8; 16], fingerprint(session, 0, &change)],
    ).unwrap();
    super::validate_steering_mutation_history(&connection).unwrap();
    for corruption in [
        "DELETE FROM mutation_requests",
        "UPDATE mutation_requests SET operation_kind = 17",
        "UPDATE mutation_requests SET accepted_sequence = 3",
        "UPDATE mutation_requests SET accepted_at_milliseconds = 101",
        "UPDATE steering_mutation_requests SET change_kind = 2",
        "UPDATE steering_mutation_requests SET text = 'changed'",
        "UPDATE steering_mutation_requests SET operation_fingerprint = zeroblob(32)",
    ] {
        connection.execute_batch("SAVEPOINT corruption").unwrap();
        connection.execute_batch(corruption).unwrap();
        assert!(
            super::validate_steering_mutation_history(&connection).is_err(),
            "accepted invalid mutation history: {corruption}"
        );
        connection
            .execute_batch("ROLLBACK TO corruption; RELEASE corruption")
            .unwrap();
    }
}

#[test]
fn steering_persisted_revisions_reject_nonpositive_values() {
    for value in [i64::MIN, -1, 0] {
        assert!(steering_revision(value).is_err());
    }
    assert_eq!(steering_revision(1).unwrap(), 1);
    assert_eq!(steering_revision(i64::MAX).unwrap(), i64::MAX as u64);
}

#[test]
fn steering_reconstruction_preserves_fifo_edits_removals_and_lifecycle_pause() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE steering_mutation_requests (
            session_id INTEGER, queue_revision INTEGER, accepted_sequence INTEGER,
            target_run_id INTEGER, change_kind INTEGER, item_id INTEGER,
            item_revision INTEGER, text TEXT, actor INTEGER, accepted_at_milliseconds INTEGER
         );
         CREATE TABLE steering_lifecycle_facts (
            session_id INTEGER, queue_revision INTEGER, fact_sequence INTEGER, target_run_id INTEGER
         );
         CREATE TABLE steering_queues (
            session_id INTEGER PRIMARY KEY, target_run_id INTEGER, revision INTEGER, paused INTEGER
         );
         CREATE TABLE steering_pending_messages (
            item_id INTEGER PRIMARY KEY, session_id INTEGER, slot INTEGER,
            enqueue_sequence INTEGER, revision INTEGER, text TEXT, actor INTEGER,
            created_at_milliseconds INTEGER, UNIQUE(session_id, slot)
         );
         INSERT INTO steering_queues VALUES (2, 20, 1, 1);
         INSERT INTO steering_pending_messages VALUES (200, 2, 9, 1, 1, 'legacy', 1, 0);
         INSERT INTO steering_mutation_requests VALUES
            (1, 1, 10, 10, 1, 100, 1, 'first', 1, 1000),
            (1, 2, 11, 10, 1, 101, 1, 'second', 1, 1001),
            (1, 3, 12, NULL, 2, 101, 2, 'edited', 1, 1002),
            (1, 4, 13, NULL, 3, 100, 1, NULL, 1, 1003),
            (1, 5, 14, 11, 5, NULL, NULL, NULL, 1, 1004);
         INSERT INTO steering_lifecycle_facts VALUES (1, 6, 15, 11);
         INSERT INTO steering_mutation_requests VALUES
            (1, 7, 16, 11, 1, 99, 1, 'third', 1, 1006),
            (1, 8, 17, NULL, 2, 101, 3, 'edited again', 1, 1007);",
        )
        .unwrap();
    for _ in 0..2 {
        steering_rebuild::rebuild(&connection).unwrap();
        let queue: (i64, i64, i64) = connection
            .query_row(
                "SELECT target_run_id, revision, paused FROM steering_queues WHERE session_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(queue, (11, 8, 1));
        let item: (i64, i64, i64, String, i64) = connection
            .query_row(
                "SELECT item_id, enqueue_sequence, revision, text, created_at_milliseconds
             FROM steering_pending_messages WHERE session_id = 1 ORDER BY slot LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(item, (101, 11, 3, "edited again".into(), 1001));
        let fifo: Vec<(i64, i64)> = connection
            .prepare("SELECT item_id, slot FROM steering_pending_messages WHERE session_id = 1 ORDER BY slot")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(fifo, [(101, 1), (99, 2)]);
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM steering_pending_messages",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 3);
        let legacy_slot: i64 = connection
            .query_row(
                "SELECT slot FROM steering_pending_messages WHERE item_id = 200",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(legacy_slot, 9);
    }
    connection
        .execute_batch(
            "UPDATE steering_pending_messages SET text = 'damaged' WHERE item_id = 101;
             CREATE TRIGGER reject_rebuild BEFORE INSERT ON steering_pending_messages
             WHEN NEW.item_id = 99
             BEGIN SELECT RAISE(ABORT, 'injected rebuild failure'); END;",
        )
        .unwrap();
    {
        let transaction = connection.unchecked_transaction().unwrap();
        assert!(steering_rebuild::rebuild(&transaction).is_err());
    }
    let retained: String = connection
        .query_row(
            "SELECT text FROM steering_pending_messages WHERE item_id = 101",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(retained, "damaged");
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM steering_pending_messages",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 3);
}

#[test]
fn steering_capacity_is_checked_at_every_historical_boundary() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE steering_mutation_requests (
                session_id INTEGER, accepted_sequence INTEGER, change_kind INTEGER
            );",
        )
        .unwrap();
    for sequence in 1..=16 {
        connection
            .execute(
                "INSERT INTO steering_mutation_requests VALUES (1, ?1, 1)",
                [sequence],
            )
            .unwrap();
    }
    validate_steering_capacity_history(&connection).unwrap();
    for (sequence, kind) in [(17, 3), (18, 1), (19, 2), (20, 4), (21, 5)] {
        connection
            .execute(
                "INSERT INTO steering_mutation_requests VALUES (1, ?1, ?2)",
                params![sequence, kind],
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO steering_mutation_requests VALUES (2, 22, 1)",
            [],
        )
        .unwrap();
    validate_steering_capacity_history(&connection).unwrap();
    // The final count is valid, but the reordered history temporarily held seventeen items.
    connection
        .execute(
            "UPDATE steering_mutation_requests SET accepted_sequence = 23
             WHERE session_id = 1 AND change_kind = 3",
            [],
        )
        .unwrap();
    assert!(validate_steering_capacity_history(&connection).is_err());
    connection
        .execute_batch(
            "DELETE FROM steering_mutation_requests;
             INSERT INTO steering_mutation_requests VALUES (1, 1, 3), (1, 2, 1);",
        )
        .unwrap();
    assert!(validate_steering_capacity_history(&connection).is_err());
}
