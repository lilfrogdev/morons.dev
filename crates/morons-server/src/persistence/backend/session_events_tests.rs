use super::notification_high_water;
use rusqlite::Connection;

#[test]
fn steering_delivery_notification_high_water_tracks_commits_not_rollbacks() {
    // Minimal query fixture; canonical delivery validation has full-schema coverage.
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE delivery_events (event_sequence INTEGER);
             CREATE TABLE steering_mutation_requests (accepted_sequence INTEGER);
             CREATE TABLE steering_delivery_facts (fact_sequence INTEGER);
             CREATE TABLE steering_lifecycle_facts (fact_sequence INTEGER);",
        )
        .unwrap();
    assert_eq!(notification_high_water(&connection).unwrap(), 0);

    for (table, sequence) in [
        ("delivery_events", 1_u32),
        ("steering_mutation_requests", 2),
        ("steering_lifecycle_facts", 3),
        ("steering_delivery_facts", 4),
    ] {
        connection
            .execute(&format!("INSERT INTO {table} VALUES (?1)"), [sequence])
            .unwrap();
        assert_eq!(
            notification_high_water(&connection).unwrap(),
            u64::from(sequence)
        );
    }

    let transaction = connection.transaction().unwrap();
    transaction
        .execute("INSERT INTO steering_delivery_facts VALUES (5)", [])
        .unwrap();
    transaction.rollback().unwrap();
    assert_eq!(notification_high_water(&connection).unwrap(), 4);

    let transaction = connection.transaction().unwrap();
    transaction
        .execute("INSERT INTO steering_delivery_facts VALUES (6)", [])
        .unwrap();
    transaction.commit().unwrap();
    assert_eq!(notification_high_water(&connection).unwrap(), 6);

    connection
        .execute("INSERT INTO steering_delivery_facts VALUES (5)", [])
        .unwrap();
    assert_eq!(notification_high_water(&connection).unwrap(), 6);
    connection
        .execute("INSERT INTO steering_lifecycle_facts VALUES (7)", [])
        .unwrap();
    assert_eq!(notification_high_water(&connection).unwrap(), 7);
}
