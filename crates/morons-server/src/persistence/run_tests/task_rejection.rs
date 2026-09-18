use super::*;
use crate::tools::{SubagentTask, ToolInput, ValidatedProviderCall};

#[tokio::test(flavor = "current_thread")]
async fn current_catalog_rejects_task_before_committing_any_tool_facts() {
    let root = TestRoot::new("retired-task-admission");
    let selected = TestRoot::new("retired-task-selected");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    configure_credential(&store).await;
    let session = store
        .create_session_at(
            MutationRequestId::from_bytes([0xe1; 16]),
            None,
            selected.path().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    let accepted = store
        .accept_session_input(
            MutationRequestId::from_bytes([0xe2; 16]),
            session.id,
            "No external effects".into(),
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
        panic!("expected prepared operation")
    };
    store
        .mark_provider_dispatched(run, operation)
        .await
        .unwrap();
    let result = store
        .complete_provider_tool_turn(
            run,
            operation,
            CompletedToolTurn {
                provider_response_id: "resp_retired_task".into(),
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
                    provider_call_id: "call_retired_task".into(),
                    input: ToolInput::Task {
                        context: "Scope".into(),
                        tasks: vec![SubagentTask {
                            name: None,
                            task: "Inspect".into(),
                        }],
                    },
                    opaque_continuation: None,
                }],
            },
        )
        .await;
    assert!(matches!(result, Err(PersistenceError::InvalidInput { .. })));
    drop(store);
    let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    for table in [
        "tool_calls",
        "task_model_bindings",
        "child_runs",
        "child_journal",
    ] {
        let count: i64 = db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "rejected turn wrote {table}");
    }
    drop(db);
    drop(SessionStore::open_for_test(root.path()).unwrap());
}
