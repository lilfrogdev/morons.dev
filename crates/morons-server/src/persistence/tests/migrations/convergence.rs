use super::*;

fn schema_objects(connection: &Connection) -> Vec<(String, String, String)> {
    connection
        .prepare(
            "SELECT type, name, sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY type, name",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn released_and_local_search_schemas_converge_with_preserved_backups() {
    let fresh_root = TestRoot::new("convergence-fresh");
    let fresh_paths = StoragePaths::prepare(fresh_root.path()).unwrap();
    let fresh = database::open(&fresh_paths).unwrap();
    let expected = schema_objects(&fresh);
    let local_steps = [
        include_str!("../../schema_v35.sql"),
        include_str!("../../schema_v36.sql"),
        include_str!("../../schema_v37.sql"),
        include_str!("../../schema_v38.sql"),
        include_str!("../../schema_v39.sql"),
        include_str!("../../schema_v40.sql"),
    ];
    for (version, local) in std::iter::once((34, false)).chain((34..=40).map(|v| (v, true))) {
        let root = TestRoot::new("schema-convergence");
        let paths = StoragePaths::prepare(root.path()).unwrap();
        let reference = database::schema_29_fixture();
        for schema in [
            include_str!("../../schema_v30.sql"),
            include_str!("../../schema_v31.sql"),
            include_str!("../../schema_v32.sql"),
            include_str!("../../schema_v33.sql"),
        ] {
            reference.execute_batch(schema).unwrap();
        }
        reference
            .execute("UPDATE logical_sequences SET next_value = 101", [])
            .unwrap();
        reference
            .execute_batch(if local {
                include_str!("../../schema_v34_search.sql")
            } else {
                include_str!("../../schema_v34.sql")
            })
            .unwrap();
        for schema in local_steps.iter().take((version - 34) as usize) {
            reference.execute_batch(schema).unwrap();
        }
        let original = schema_objects(&reference);
        let (initialization_path, file) = paths
            .create_database_initialization_file(&[0xa1; 16])
            .unwrap();
        drop(file);
        reference
            .execute("VACUUM INTO ?1", [initialization_path.to_str().unwrap()])
            .unwrap();
        drop(reference);
        paths.install_database(&initialization_path).unwrap();

        let migrated = database::open(&paths).unwrap();
        assert_eq!(
            schema_objects(&migrated),
            expected,
            "version {version}, local {local}"
        );
        assert_eq!(pragma_integer(&migrated, "PRAGMA foreign_keys"), 1);
        assert_eq!(
            pragma_integer(&migrated, "SELECT COUNT(*) FROM pragma_foreign_key_check"),
            0
        );
        let integrity: String = migrated
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        for query in [
            "SELECT first_sequence FROM web_attempt_epoch",
            "SELECT first_sequence FROM web_provenance_epoch",
        ] {
            assert_eq!(pragma_integer(&migrated, query), 101);
        }
        assert_eq!(
            pragma_integer(
                &migrated,
                "SELECT repeated_first_sequence FROM context_accounting_epoch"
            ),
            101
        );
        let backup_path = paths.migration_backup_path(version).unwrap();
        let backup_bytes = fs::read(&backup_path).unwrap();
        let backup =
            Connection::open_with_flags(&backup_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        assert_eq!(pragma_integer(&backup, "PRAGMA user_version"), version);
        assert_eq!(schema_objects(&backup), original);
        drop(backup);
        drop(migrated);
        let reopened = database::open(&paths).unwrap();
        assert_eq!(schema_objects(&reopened), expected);
        assert_eq!(fs::read(&backup_path).unwrap(), backup_bytes);
    }
}
