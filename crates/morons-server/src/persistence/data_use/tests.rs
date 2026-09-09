mod ipc;
mod policy;

use rusqlite::Connection;

pub(crate) fn restore_schema_29(connection: &Connection) {
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM task_model_bindings", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let reference = super::super::database::schema_29_fixture();
    connection.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE; DROP TABLE web_model_bindings; DROP TABLE web_binding_epoch; DROP TABLE task_model_bindings; DROP TABLE provider_binding_epoch;").unwrap();
    restore_tables(
        connection,
        &reference,
        &[
            "context_checkpoints",
            "default_model_selections",
            "provider_operation_facts",
            "run_accepted_facts",
            "run_state_facts",
            "runs",
            "session_entries",
            "subagent_model_selections",
        ],
    );
    connection
        .execute_batch("PRAGMA user_version = 29; COMMIT; PRAGMA foreign_keys = ON;")
        .unwrap();
    assert!(
        !connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)",
                [],
                |row| row.get::<_, bool>(0)
            )
            .unwrap()
    );
}

fn restore_tables(connection: &Connection, reference: &Connection, tables: &[&str]) {
    for table in tables {
        let ddl: String = reference
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name=?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        let prefix = ddl.find('(').unwrap();
        connection.execute_batch(&format!("CREATE TABLE {table}_restore {}; INSERT INTO {table}_restore SELECT * FROM {table}; DROP TABLE {table}; ALTER TABLE {table}_restore RENAME TO {table};", &ddl[prefix..])).unwrap();
        let mut statement=reference.prepare("SELECT sql FROM sqlite_schema WHERE type='index' AND tbl_name=?1 AND sql IS NOT NULL").unwrap();
        for index in statement
            .query_map([table], |row| row.get::<_, String>(0))
            .unwrap()
        {
            connection.execute_batch(&index.unwrap()).unwrap();
        }
    }
}

/// Restore an exact empty-policy v28 fixture before an older migration setup.
pub(crate) fn restore_schema_28(connection: &Connection) {
    restore_schema_29(connection);
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM data_use_policies", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
    let reference = super::super::database::schema_28_fixture();
    connection
        .execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")
        .unwrap();
    connection.execute_batch("DROP TABLE data_use_policies; ALTER TABLE compaction_maintenance_jobs DROP COLUMN data_use_sequence;").unwrap();
    restore_tables(
        connection,
        &reference,
        &[
            "mutation_requests",
            "run_state_facts",
            "runs",
            "provider_operation_facts",
        ],
    );
    connection
        .execute_batch("PRAGMA user_version = 28; COMMIT; PRAGMA foreign_keys = ON;")
        .unwrap();
    assert!(
        !connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)",
                [],
                |row| row.get::<_, bool>(0)
            )
            .unwrap()
    );
}
