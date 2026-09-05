use super::*;

#[test]
fn context_records_reject_missing_extra_rebound_oversized_and_unknown_data() {
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("CREATE TABLE run_accepted_facts (run_id BLOB PRIMARY KEY, tool_catalog_version INTEGER);").unwrap();
    connection
        .execute_batch(include_str!("../../schema_v26.sql"))
        .unwrap();
    let legacy = RunId::from_bytes([1; 16]);
    let current = RunId::from_bytes([2; 16]);
    for (id, version) in [(legacy, 8), (current, 9)] {
        connection
            .execute(
                "INSERT INTO run_accepted_facts VALUES (?1, ?2)",
                params![&id.as_bytes()[..], version],
            )
            .unwrap();
    }
    assert!(load(&connection, legacy).unwrap().is_none());
    assert!(load(&connection, current).is_err());
    let context = RunProjectContext::default();
    insert(&connection, current, &context).unwrap();
    assert_eq!(load(&connection, current).unwrap(), Some(context.clone()));
    insert(&connection, legacy, &context).unwrap();
    assert!(load(&connection, legacy).is_err());
    connection.execute("UPDATE run_project_contexts SET source_digest = (SELECT source_digest FROM run_project_contexts WHERE run_id = ?1) WHERE run_id = ?2", params![&legacy.as_bytes()[..], &current.as_bytes()[..]]).unwrap();
    assert!(load(&connection, current).is_err());
    let snapshot = r#"{"enabled":true,"files":[],"warnings":[],"unexpected":"field"}"#;
    connection
        .execute(
            "UPDATE run_project_contexts SET snapshot = ?1, source_digest = ?2 WHERE run_id = ?3",
            params![
                snapshot,
                &digest(current, snapshot)[..],
                &current.as_bytes()[..]
            ],
        )
        .unwrap();
    assert!(load(&connection, current).is_err());
    connection
        .execute_batch("PRAGMA ignore_check_constraints = ON")
        .unwrap();
    connection
        .execute(
            "UPDATE run_project_contexts SET snapshot = ?1 WHERE run_id = ?2",
            params!["x".repeat(MAX_SNAPSHOT_BYTES + 1), &current.as_bytes()[..]],
        )
        .unwrap();
    assert!(load(&connection, current).is_err());
}
