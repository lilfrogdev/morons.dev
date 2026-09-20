use super::*;

fn fixture() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE session_created_facts (session_id BLOB PRIMARY KEY);
             CREATE TABLE run_accepted_facts (session_id BLOB, run_id BLOB UNIQUE);
             INSERT INTO session_created_facts VALUES (zeroblob(16)), (randomblob(16));
             INSERT INTO run_accepted_facts VALUES (zeroblob(16), zeroblob(16));",
        )
        .unwrap();
    connection
        .execute_batch(include_str!("../../schema_v43.sql"))
        .unwrap();
    connection
        .execute(
            "INSERT INTO steering_queues (session_id, target_run_id, revision)
             VALUES (zeroblob(16), zeroblob(16), 1)",
            [],
        )
        .unwrap();
    connection
}

fn insert(
    connection: &Connection,
    slot: i64,
    sequence: i64,
    text: &str,
) -> rusqlite::Result<usize> {
    connection.execute(
        "INSERT INTO steering_pending_messages
         (item_id, session_id, slot, enqueue_sequence, revision, text, actor,
          created_at_milliseconds)
         VALUES (randomblob(16), zeroblob(16), ?1, ?2, 1, ?3, 1, 0)",
        params![slot, sequence, text],
    )
}

#[test]
fn steering_schema_bounds_pending_text_and_defaults_to_paused() {
    let connection = fixture();
    assert_eq!(
        pragma_integer(&connection, "SELECT paused FROM steering_queues"),
        1
    );
    assert!(insert(&connection, 1, 1, "").is_err());
    assert!(insert(&connection, 1, 1, &"é".repeat(32769)).is_err());
    let maximum = "é".repeat(32768);
    for slot in 1..=16 {
        insert(&connection, slot, 17 - slot, &maximum).unwrap();
    }
    assert!(insert(&connection, 17, 17, "overflow").is_err());
    assert!(insert(&connection, 0, 17, "invalid slot").is_err());
    assert!(insert(&connection, 1, 17, "duplicate slot").is_err());
    assert_eq!(
        pragma_integer(
            &connection,
            "SELECT sum(length(CAST(text AS BLOB))) FROM steering_pending_messages"
        ),
        1024 * 1024
    );
    assert_eq!(
        pragma_integer(
            &connection,
            "SELECT slot FROM steering_pending_messages ORDER BY enqueue_sequence LIMIT 1"
        ),
        16
    );
}

#[test]
fn steering_schema_rejects_cross_session_targets_and_invalid_records() {
    let connection = fixture();
    assert!(connection.execute(
        "INSERT INTO steering_queues (session_id, target_run_id, revision)
         SELECT session_id, zeroblob(16), 1 FROM session_created_facts WHERE session_id != zeroblob(16)",
        [],
    ).is_err());
    insert(&connection, 1, 1, "pending").unwrap();
    for statement in [
        "UPDATE steering_queues SET revision = 0",
        "UPDATE steering_queues SET paused = 2",
        "UPDATE steering_pending_messages SET actor = 2",
        "UPDATE steering_pending_messages SET revision = 0",
        "UPDATE steering_pending_messages SET enqueue_sequence = 0",
        "UPDATE steering_pending_messages SET item_id = zeroblob(15)",
        "UPDATE steering_pending_messages SET created_at_milliseconds = -1",
    ] {
        assert!(connection.execute(statement, []).is_err(), "{statement}");
    }
    assert!(insert(&connection, 2, 1, "duplicate sequence").is_err());
    connection
        .execute("DELETE FROM steering_queues", [])
        .unwrap();
    assert_eq!(
        pragma_integer(
            &connection,
            "SELECT count(*) FROM steering_pending_messages"
        ),
        0
    );
    assert_eq!(
        pragma_integer(&connection, "SELECT count(*) FROM run_accepted_facts"),
        1
    );
}
