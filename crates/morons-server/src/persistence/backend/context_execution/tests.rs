use super::*;

mod native;

#[tokio::test(flavor = "current_thread")]
async fn schema32_migration_preserves_compactions_and_legacy_execution() {
    use super::super::context_compaction::tests::{append_stopped, fixture};
    use crate::persistence::{MutationRequestId, RunModelSelection, RunService, SessionStore};
    let (root, _selected, store, session) = fixture("context-policy-migration").await;
    for index in 1..=3 {
        append_stopped(&store, session, index, format!("OLD-{index}")).await;
    }
    let accepted = store
        .accept_session_input(
            MutationRequestId::from_bytes([4; 16]),
            session,
            "/compact".into(),
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
        .unwrap();
    store.activate_run(accepted.run.id).await.unwrap();
    let context = store.load_run_context(accepted.run.id).await.unwrap();
    let plan = context.compaction_plan.unwrap();
    let operation = store
        .prepare_auto_compaction(accepted.run.id, &plan)
        .await
        .unwrap();
    store
        .mark_compaction_dispatched(accepted.run.id, operation)
        .await
        .unwrap();
    store
        .complete_compaction(
            accepted.run.id,
            operation,
            RunService::Zen,
            "muse-spark-1.2".into(),
            "SUMMARY".into(),
        )
        .await
        .unwrap();
    store
        .finish_run_stopped(accepted.run.id, None)
        .await
        .unwrap();
    drop(store);
    let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    let tables = [
        "run_accepted_facts",
        "run_state_facts",
        "session_entries",
        "compaction_operations",
        "context_checkpoints",
        "credential_audit_facts",
    ];
    let before: Vec<_> = tables.iter().map(|table| rows(&db, table)).collect();
    crate::persistence::data_use::tests::restore_schema_32(&db);
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        32
    );
    drop(db);
    let migrated = SessionStore::open_for_test(root.path()).unwrap();
    drop(migrated);
    let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        33
    );
    assert_eq!(
        before,
        tables
            .iter()
            .map(|table| rows(&db, table))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        policy(&db, accepted.run.id).unwrap(),
        ExecutionPolicy::Legacy
    );
    let backup = Connection::open(
        root.path()
            .join("backups/sessions-before-schema-v32.sqlite3"),
    )
    .unwrap();
    assert_eq!(
        backup
            .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        32
    );
    assert_eq!(
        before,
        tables
            .iter()
            .map(|table| rows(&backup, table))
            .collect::<Vec<_>>()
    );
}

fn rows(db: &Connection, table: &str) -> Vec<Vec<rusqlite::types::Value>> {
    let mut stmt = db
        .prepare(&format!("SELECT * FROM {table} ORDER BY 1,2"))
        .unwrap();
    let columns = stmt.column_count();
    stmt.query_map([], |row| {
        (0..columns)
            .map(|column| row.get(column))
            .collect::<rusqlite::Result<Vec<_>>>()
    })
    .unwrap()
    .collect::<rusqlite::Result<Vec<_>>>()
    .unwrap()
}

#[test]
fn usage_admission_keeps_independent_resources_and_legacy_fallback() {
    let budget = ContextBudget {
        bytes: 96_000,
        entries: 8,
        observed_input_tokens: Some(20_000),
        ..ContextBudget::default()
    };
    assert!(!ExecutionPolicy::Legacy.fits(&budget, 96_000, 0));
    assert!(ExecutionPolicy::NativeUsage.fits(&budget, 96_000, 0));
    for budget in [
        ContextBudget {
            bytes: MAX_SOURCE_BYTES + 1,
            observed_input_tokens: Some(1),
            ..ContextBudget::default()
        },
        ContextBudget {
            bytes: u64::MAX,
            observed_input_tokens: Some(1),
            ..ContextBudget::default()
        },
        ContextBudget {
            entries: 233,
            observed_input_tokens: Some(1),
            ..ContextBudget::default()
        },
        ContextBudget {
            images: 17,
            observed_input_tokens: Some(1),
            ..ContextBudget::default()
        },
        ContextBudget {
            image_bytes: crate::persistence::images::MAX_CONTEXT_IMAGE_BYTES + 1,
            observed_input_tokens: Some(1),
            ..ContextBudget::default()
        },
        ContextBudget {
            bytes: 96_000,
            images: 1,
            observed_input_tokens: Some(1),
            ..ContextBudget::default()
        },
        ContextBudget {
            bytes: 96_000,
            ..ContextBudget::default()
        },
    ] {
        assert!(!ExecutionPolicy::NativeUsage.fits(&budget, 96_000, 0));
    }
    let exact = ContextBudget {
        bytes: MAX_SOURCE_BYTES,
        entries: 232,
        observed_input_tokens: Some(96_000),
        ..ContextBudget::default()
    };
    assert!(ExecutionPolicy::NativeUsage.fits(&exact, 96_000, 0));
    assert!(!ExecutionPolicy::NativeUsage.fits(&exact, 96_000, 1));
    assert!(ExecutionPolicy::NativeUsage.pressure(&exact, 96_000, 0));
}

#[test]
fn batch_cut_rejects_missing_and_crossing_results() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE tool_calls(call_id INTEGER, run_id BLOB, provider_operation_id INTEGER); CREATE TABLE session_entries(tool_call_id INTEGER, entry_kind INTEGER, entry_sequence INTEGER);
      INSERT INTO tool_calls VALUES(1,x'01010101010101010101010101010101',10),(2,x'01010101010101010101010101010101',10),(3,x'01010101010101010101010101010101',11);
      INSERT INTO session_entries VALUES(1,3,2),(2,3,3),(1,4,4),(2,4,5),(3,3,6);").unwrap();
    let run = RunId::from_bytes([1; 16]);
    for cut in [2, 3, 4, 6] {
        assert!(!complete_cut(&db, run, cut).unwrap());
    }
    assert!(complete_cut(&db, run, 5).unwrap());
    db.execute("INSERT INTO session_entries VALUES(3,4,7)", [])
        .unwrap();
    assert!(complete_cut(&db, run, 7).unwrap());
}
