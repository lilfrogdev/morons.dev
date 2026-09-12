use super::*;
use crate::tools::{SubagentTask, ToolErrorKind, ToolInput, ToolPath, ValidatedProviderCall};

#[tokio::test(flavor = "current_thread")]
async fn deletion_of_ordinary_tool_history_uses_no_cascade_triggers() {
    delete_history(false, false).await;
}
#[tokio::test(flavor = "current_thread")]
async fn deletion_of_bound_task_history_uses_no_cascade_triggers() {
    delete_history(true, false).await;
}
#[tokio::test(flavor = "current_thread")]
async fn schema_30_task_binding_migration_preserves_source_and_removes_only_cascade() {
    delete_history(true, true).await;
}

async fn delete_history(task: bool, migrate: bool) {
    let root = TestRoot::new("tool-delete");
    let selected = TestRoot::new("tool-delete-selected");
    fs::write(selected.path().join("keep.txt"), "keep").unwrap();
    let store = SessionStore::open_for_test(root.path()).unwrap();
    configure_credential(&store).await;
    let session = store
        .create_session_at(
            MutationRequestId::from_bytes([0xd1; 16]),
            None,
            selected.path().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    let accepted = store
        .accept_session_input(
            MutationRequestId::from_bytes([0xd2; 16]),
            session.id,
            "Fixture no external effects".into(),
            model_selection(),
        )
        .await
        .unwrap();
    let run = accepted.run.id;
    store.activate_run(run).await.unwrap();
    let context = store.load_run_context(run).await.unwrap();
    let PrepareOperationOutcome::Prepared(operation) = store
        .prepare_provider_operation(
            run,
            context.current_entry_high_water,
            context.estimated_input_tokens,
        )
        .await
        .unwrap()
    else {
        panic!("prepared")
    };
    store
        .mark_provider_dispatched(run, operation)
        .await
        .unwrap();
    let input = if task {
        ToolInput::Task {
            context: "Fixture".into(),
            tasks: vec![SubagentTask {
                name: None,
                task: "No effects".into(),
            }],
        }
    } else {
        ToolInput::Read {
            path: ToolPath::parse("keep.txt").unwrap(),
            offset: 1,
            limit: 1,
        }
    };
    let mut turn = store
        .complete_provider_tool_turn(
            run,
            operation,
            CompletedToolTurn {
                provider_response_id: "resp_delete_fixture".into(),
                usage: ProviderUsage {
                    input_tokens: 10,
                    cached_input_tokens: 0,
                    cache_write_input_tokens: 0,
                    output_tokens: 2,
                    reasoning_output_tokens: 0,
                    total_tokens: 12,
                },
                commentary: None,
                calls: vec![ValidatedProviderCall {
                    provider_call_id: "call_delete_fixture".into(),
                    input,
                    opaque_continuation: None,
                }],
            },
        )
        .await
        .unwrap();
    let call = turn.calls.remove(0);
    store
        .prepare_tool_operation(run, call.call_id, call.operation_id, None)
        .await
        .unwrap();
    store
        .mark_tool_dispatched(run, call.call_id, call.operation_id)
        .await
        .unwrap();
    store
        .complete_tool_result(
            run,
            call.call_id,
            call.operation_id,
            ToolResult::error(ToolErrorKind::Interrupted),
        )
        .await
        .unwrap();
    store.finish_run_stopped(run, None).await.unwrap();
    let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM task_model_bindings", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        i64::from(task)
    );
    let store = if migrate {
        let before = format!(
            "{:?}",
            store.task_model_binding(run, call.call_id).await.unwrap()
        );
        drop(store);
        crate::persistence::data_use::tests::restore_schema_32(&db);
        db.execute_batch(
            "PRAGMA foreign_keys=OFF; DROP TABLE web_model_bindings; DROP TABLE web_binding_epoch;",
        )
        .unwrap();
        let original = include_str!("../schema_v30.sql");
        let start = original.find("CREATE TABLE task_model_bindings (").unwrap();
        let end = original[start..].find("PRAGMA user_version").unwrap() + start;
        db.execute_batch("PRAGMA foreign_keys=OFF; BEGIN IMMEDIATE; ALTER TABLE task_model_bindings RENAME TO task_binding_fixture; DROP INDEX task_model_bindings_by_run;").unwrap();
        db.execute_batch(&original[start..end]).unwrap();
        db.execute_batch("INSERT INTO task_model_bindings SELECT * FROM task_binding_fixture; DROP TABLE task_binding_fixture; PRAGMA user_version=30; COMMIT;").unwrap();
        let store = SessionStore::open_for_test(root.path()).unwrap();
        assert_eq!(
            format!(
                "{:?}",
                store.task_model_binding(run, call.call_id).await.unwrap()
            ),
            before
        );
        let ddl: String = db
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name='task_model_bindings'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!ddl.contains("CASCADE"));
        assert!(
            root.path()
                .join("backups/sessions-before-schema-v30.sqlite3")
                .exists()
        );
        store
    } else {
        store
    };
    store
        .set_session_archived(MutationRequestId::from_bytes([0xd3; 16]), session.id, true)
        .await
        .unwrap();
    store
        .delete_session(MutationRequestId::from_bytes([0xd4; 16]), session.id)
        .await
        .unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM task_model_bindings", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        fs::read_to_string(selected.path().join("keep.txt")).unwrap(),
        "keep"
    );
    drop(store);
    assert!(SessionStore::open_for_test(root.path()).is_ok());
}
