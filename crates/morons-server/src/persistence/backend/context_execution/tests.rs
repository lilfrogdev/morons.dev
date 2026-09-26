use super::*;

#[test]
fn native_capacity_preserves_old_pressure_and_independent_bounds() {
    let policy = ExecutionPolicy::NativeUsage;
    let maximum = crate::provider::openai_codex::USABLE_INPUT_TOKENS;
    assert_eq!(policy.token_pressure_threshold(96_000), 67_200);
    assert_eq!(policy.token_pressure_threshold(maximum), 244_800);
    let mut budget = ContextBudget {
        observed_input_tokens: Some(244_799),
        ..Default::default()
    };
    assert!(!policy.pressure(&budget, maximum, 0));
    budget.observed_input_tokens = Some(244_800);
    assert!(policy.pressure(&budget, maximum, 0));
    budget.observed_input_tokens = Some(u64::from(maximum));
    assert!(policy.fits(&budget, maximum, 0));
    budget.observed_input_tokens = Some(u64::from(maximum) + 1);
    assert!(!policy.fits(&budget, maximum, 0));
    budget.observed_input_tokens = Some(1);
    budget.bytes = MAX_SOURCE_BYTES + 1;
    assert!(!policy.fits(&budget, maximum, 0));
}

#[test]
fn rejected_budget_diagnostics_match_policy_guards() {
    let run = RunId::from_bytes([7; 16]);
    for policy in [
        ExecutionPolicy::Legacy,
        ExecutionPolicy::ConservativeRepeated,
        ExecutionPolicy::NativeUsage,
    ] {
        for budget in [
            ContextBudget::default(),
            ContextBudget {
                bytes: MAX_SOURCE_BYTES + 1,
                observed_input_tokens: Some(1),
                ..Default::default()
            },
            ContextBudget {
                entries: 233,
                ..Default::default()
            },
            ContextBudget {
                images: 100,
                ..Default::default()
            },
            ContextBudget {
                image_bytes: u64::MAX,
                ..Default::default()
            },
        ] {
            let events: Vec<_> = policy
                .rejected_budget_events(run, &budget, 96_000, 0)
                .collect();
            assert_eq!(events.is_empty(), policy.fits(&budget, 96_000, 0));
            for event in events {
                let crate::debug_log::DebugEvent::ContextLimit {
                    run_id,
                    call_id,
                    measured,
                    limit,
                    ..
                } = event
                else {
                    panic!("unexpected event")
                };
                assert_eq!(run_id, [7; 16]);
                assert_eq!(call_id, None);
                assert!(measured > limit);
            }
        }
        let boundary = ContextBudget {
            entries: 232,
            ..Default::default()
        };
        assert_eq!(
            policy
                .rejected_budget_events(run, &boundary, 96_000, 0)
                .count(),
            0
        );
    }
}

mod native;

#[tokio::test(flavor = "current_thread")]
async fn schema32_migration_preserves_compactions_and_legacy_execution() {
    migration_preserves_compactions(32).await;
}

#[tokio::test(flavor = "current_thread")]
async fn schema33_migration_preserves_compactions_and_accounting_epoch() {
    migration_preserves_compactions(33).await;
}

async fn migration_preserves_compactions(version: u32) {
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
    let mut tables = vec![
        "run_accepted_facts",
        "run_state_facts",
        "session_entries",
        "compaction_operations",
        "context_checkpoints",
        "credential_audit_facts",
    ];
    if version == 33 {
        tables.extend([
            "context_accounting_epoch",
            "provider_operation_facts",
            "logical_sequences",
        ]);
    }
    let before: Vec<_> = tables.iter().map(|table| rows(&db, table)).collect();
    if version == 33 {
        crate::persistence::data_use::tests::restore_schema_33(&db);
    } else {
        crate::persistence::data_use::tests::restore_schema_32(&db);
    }
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        version
    );
    drop(db);
    let migrated = SessionStore::open_for_test(root.path()).unwrap();
    drop(migrated);
    let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        crate::persistence::database::SCHEMA_VERSION as u32
    );
    for (table, expected) in tables.iter().zip(&before) {
        assert_eq!(expected, &rows(&db, table), "migrated table {table}");
    }
    assert_eq!(
        db.query_row(
            "SELECT repeated_first_sequence FROM context_accounting_epoch",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        db.query_row("SELECT next_value FROM logical_sequences", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap()
    );
    assert_eq!(
        policy(&db, accepted.run.id).unwrap(),
        ExecutionPolicy::Legacy
    );
    let backup = Connection::open(
        root.path()
            .join(format!("backups/sessions-before-schema-v{version}.sqlite3")),
    )
    .unwrap();
    assert_eq!(
        backup
            .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        version
    );
    for (table, expected) in tables.iter().zip(&before) {
        assert_eq!(expected, &rows(&backup, table), "backup table {table}");
    }
}

fn rows(db: &Connection, table: &str) -> Vec<Vec<rusqlite::types::Value>> {
    // Schema 40 adds a migration-time boundary, not part of schema 33's evidence.
    let columns = if table == "context_accounting_epoch" {
        "singleton, first_sequence"
    } else {
        "*"
    };
    let mut stmt = db
        .prepare(&format!("SELECT {columns} FROM {table} ORDER BY 1,2"))
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
    assert!(!ExecutionPolicy::ConservativeRepeated.fits(&budget, 96_000, 0));
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
        assert!(!ExecutionPolicy::ConservativeRepeated.fits(&budget, 96_000, 0));
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
