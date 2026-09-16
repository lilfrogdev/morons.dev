use super::*;
use crate::persistence::{WebInvocation, WebRoute};
use sha2::Sha256;

fn evidence(db: &Connection) -> Vec<Vec<rusqlite::types::Value>> {
    let mut result = Vec::new();
    for table in [
        "web_model_bindings",
        "web_search_attempts",
        "web_search_successes",
        "web_binding_provenance",
        "web_attempt_epoch",
        "web_provenance_epoch",
        "context_accounting_epoch",
    ] {
        let mut statement = db
            .prepare(&format!("SELECT * FROM {table} ORDER BY 1,2"))
            .unwrap();
        let columns = statement.column_count();
        result.extend(
            statement
                .query_map([], |row| (0..columns).map(|i| row.get(i)).collect())
                .unwrap()
                .collect::<Result<Vec<Vec<_>>, _>>()
                .unwrap(),
        );
    }
    result
}

#[tokio::test]
async fn search_sqlite_overflow_rolls_back_evidence_and_sequence() {
    let root = TestRoot::new("search-overflow");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    let (run, call) = prepare(&store, false, true).await;
    store
        .mark_tool_dispatched(run, call.call_id, call.operation_id)
        .await
        .unwrap();
    let binding = store.web_binding(run, call.call_id).await.unwrap();
    let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    let invocation = WebInvocation {
        child: 1,
        ordinal: 1,
        query_digest: Sha256::digest(b"public query").into(),
        route: WebRoute::ExaMissingCredential,
    };
    for (table, counter) in [
        ("web_search_attempts", "admission_count"),
        ("web_search_successes", "success_count"),
    ] {
        let before = evidence(&db);
        let sequence: i64 = db
            .query_row("SELECT next_value FROM logical_sequences", [], |r| r.get(0))
            .unwrap();
        // Inject the boundary inside the transaction, after admission integrity validation.
        db.execute_batch(&format!("CREATE TRIGGER inject_overflow BEFORE INSERT ON {table} BEGIN UPDATE web_model_bindings SET {counter}=9223372036854775807; END;")).unwrap();
        if counter == "admission_count" {
            assert!(
                store
                    .dispatch_web_search(&binding, invocation.clone())
                    .await
                    .is_err()
            );
        } else {
            assert!(
                store
                    .complete_web_search(&binding, invocation.clone(), successful_web_result(false))
                    .await
                    .is_err()
            );
        }
        assert_eq!(evidence(&db), before);
        assert_eq!(
            db.query_row("SELECT next_value FROM logical_sequences", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            sequence
        );
        db.execute_batch("DROP TRIGGER inject_overflow").unwrap();
        if counter == "admission_count" {
            store
                .dispatch_web_search(&binding, invocation.clone())
                .await
                .unwrap();
        }
    }
    let before = evidence(&db);
    let sequence: i64 = db
        .query_row("SELECT next_value FROM logical_sequences", [], |r| r.get(0))
        .unwrap();
    for ordinal in [i64::MAX as u64 + 1, u64::MAX] {
        let mut too_wide = invocation.clone();
        too_wide.ordinal = ordinal;
        assert!(store.dispatch_web_search(&binding, too_wide).await.is_err());
        assert_eq!(evidence(&db), before);
        assert_eq!(
            db.query_row("SELECT next_value FROM logical_sequences", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            sequence
        );
    }
}

#[tokio::test]
async fn schema_41_preserves_populated_search_evidence_and_accepts_wide_ordinals() {
    for (native, historical) in [(false, false), (true, false), (false, true), (true, true)] {
        let root = TestRoot::new("populated-search-convergence");
        let store = SessionStore::open_for_test(root.path()).unwrap();
        if native {
            install(&store, 0).await;
        }
        let (run, call) = prepare(&store, native, true).await;
        store
            .mark_tool_dispatched(run, call.call_id, call.operation_id)
            .await
            .unwrap();
        let binding = store.web_binding(run, call.call_id).await.unwrap();
        let ordinals: &[u64] = if historical {
            &[1, 24]
        } else {
            &[24, 25, u16::MAX as u64, u16::MAX as u64 + 1]
        };
        for &ordinal in ordinals {
            store
                .dispatch_web_search(
                    &binding,
                    WebInvocation {
                        child: 1,
                        ordinal,
                        query_digest: Sha256::digest(b"public query").into(),
                        route: if native {
                            WebRoute::OpenAi
                        } else {
                            WebRoute::ExaMissingCredential
                        },
                    },
                )
                .await
                .unwrap();
            record_success(&store, &binding, native, 1, ordinal).await;
        }
        store
            .complete_tool_result(
                run,
                call.call_id,
                call.operation_id,
                ToolResult::error(ToolErrorKind::Cancelled),
            )
            .await
            .unwrap();
        store.finish_run_stopped(run, None).await.unwrap();
        drop(store);
        let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        let before = evidence(&db);
        if historical {
            crate::persistence::data_use::tests::restore_schema_40(&db);
        } else {
            // Exercise the table rebuild without rewriting already committed evidence.
            crate::persistence::data_use::tests::remove_child_schema(&db);
            db.pragma_update(None, "foreign_keys", false).unwrap();
            db.execute_batch(include_str!("../../../schema_v41.sql"))
                .unwrap();
        }
        drop(db);
        let store = SessionStore::open_for_test(root.path()).unwrap();
        let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        assert_eq!(evidence(&db), before);
        if historical {
            let backup = Connection::open(
                root.path()
                    .join("backups/sessions-before-schema-v40.sqlite3"),
            )
            .unwrap();
            assert_eq!(evidence(&backup), before);
            assert_eq!(
                backup
                    .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                40
            );
        }
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            0
        );
        drop(db);
        drop(store);
        assert!(SessionStore::open_for_test(root.path()).is_ok());
    }
}
