use super::*;
use crate::{
    persistence::{RunId, SessionId, run_types::ProviderOperationId},
    tools::{SubagentTask, TextReplacement, ToolInput, ToolPath, ValidatedProviderCall},
};

async fn prepared(store: &SessionStore) -> (SessionId, RunId, ProviderOperationId) {
    configure_credential(store).await;
    let session = store
        .create_session(MutationRequestId::from_bytes([0x81; 16]), None)
        .await
        .unwrap();
    let accepted = store
        .accept_session_input(
            MutationRequestId::from_bytes([0x82; 16]),
            session.id,
            "counter fixture".into(),
            model_selection(),
        )
        .await
        .unwrap();
    store.activate_run(accepted.run.id).await.unwrap();
    let context = store.load_run_context(accepted.run.id).await.unwrap();
    let PrepareOperationOutcome::Prepared(operation) = store
        .prepare_provider_operation(
            accepted.run.id,
            context.current_entry_high_water,
            context.estimated_input_tokens,
        )
        .await
        .unwrap()
    else {
        panic!("expected prepared operation");
    };
    (session.id, accepted.run.id, operation)
}

#[tokio::test(flavor = "current_thread")]
async fn run_counters_cross_former_turn_call_mutation_and_task_quotas() {
    let root = TestRoot::new("counter-uncapped");
    let store = SessionStore::open_at(root.path()).unwrap();
    let (session, run, mut operation) = prepared(&store).await;
    for turn_index in 0..33 {
        if turn_index > 0 {
            let context = store.load_run_context(run).await.unwrap();
            let PrepareOperationOutcome::Prepared(next) = store
                .prepare_provider_operation(
                    run,
                    context.current_entry_high_water,
                    context.estimated_input_tokens,
                )
                .await
                .unwrap()
            else {
                panic!("expected prepared operation beyond former quotas");
            };
            operation = next;
        }
        store
            .mark_provider_dispatched(run, operation)
            .await
            .unwrap();
        let committed = store
            .complete_provider_tool_turn(
                run,
                operation,
                CompletedToolTurn {
                    provider_response_id: format!("resp_{turn_index}"),
                    usage: usage(),
                    commentary: None,
                    calls: (0..3)
                        .map(|index| ValidatedProviderCall {
                            provider_call_id: format!("call_{turn_index}_{index}"),
                            input: if index == 0 {
                                ToolInput::Task {
                                    context: "fixture".into(),
                                    tasks: vec![SubagentTask {
                                        name: None,
                                        task: "fixture".into(),
                                    }],
                                }
                            } else {
                                ToolInput::Bash {
                                    command: "fixture only; never executed".into(),
                                }
                            },
                            opaque_continuation: None,
                        })
                        .collect(),
                },
            )
            .await
            .unwrap();
        for call in committed.calls {
            store
                .prepare_tool_operation(run, call.call_id, call.operation_id, None)
                .await
                .unwrap();
            store
                .complete_tool_result(
                    run,
                    call.call_id,
                    call.operation_id,
                    crate::tools::ToolResult::error(crate::tools::ToolErrorKind::ResourceLimit),
                )
                .await
                .unwrap();
        }
    }
    let live = store.get_run(session, run).await.unwrap().unwrap();
    assert_eq!(
        (live.provider_turns, live.tool_calls, live.tool_mutations),
        (33, 99, 99)
    );
    store.finish_run_stopped(run, None).await.unwrap();
    drop(store);
    let store = SessionStore::open_at(root.path()).unwrap();
    let recovered = store.get_run(session, run).await.unwrap().unwrap();
    assert_eq!(
        (
            recovered.provider_turns,
            recovered.tool_calls,
            recovered.tool_mutations
        ),
        (33, 99, 99)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cumulative_results_preserve_payloads_beyond_two_mib() {
    let root = TestRoot::new("counter-result-bytes");
    let store = SessionStore::open_at(root.path()).unwrap();
    let (session, run, operation) = prepared(&store).await;
    let mut expected_bytes = 0;
    store
        .mark_provider_dispatched(run, operation)
        .await
        .unwrap();
    let committed = store
        .complete_provider_tool_turn(
            run,
            operation,
            CompletedToolTurn {
                provider_response_id: "resp_large_results".into(),
                usage: usage(),
                commentary: None,
                calls: (0..8)
                    .map(|index| ValidatedProviderCall {
                        provider_call_id: format!("large_result_{index}"),
                        input: ToolInput::Bash {
                            command: "fixture only; never executed".into(),
                        },
                        opaque_continuation: None,
                    })
                    .collect(),
            },
        )
        .await
        .unwrap();
    // JSON-escaped tabs cross the serialized-byte quota while each stream stays bounded.
    let result = ToolResult::Ok {
        output: ToolOutput::Bash {
            exit_code: Some(0),
            signal: None,
            stdout: "\t".repeat(crate::tools::MAX_BASH_OUTPUT_BYTES),
            stderr: "\t".repeat(crate::tools::MAX_BASH_OUTPUT_BYTES),
        },
    };
    for call in committed.calls {
        store
            .prepare_tool_operation(run, call.call_id, call.operation_id, None)
            .await
            .unwrap();
        store
            .mark_tool_dispatched(run, call.call_id, call.operation_id)
            .await
            .unwrap();
        store
            .complete_tool_result(run, call.call_id, call.operation_id, result.clone())
            .await
            .unwrap();
        expected_bytes += serde_json::to_vec(&result).unwrap().len() as u64;
    }
    assert!(expected_bytes > 2 * 1024 * 1024);
    assert_eq!(
        store
            .get_run(session, run)
            .await
            .unwrap()
            .unwrap()
            .tool_result_bytes,
        expected_bytes
    );
    store.finish_run_stopped(run, None).await.unwrap();
    drop(store);
    let store = SessionStore::open_at(root.path()).unwrap();
    assert_eq!(
        store
            .get_run(session, run)
            .await
            .unwrap()
            .unwrap()
            .tool_result_bytes,
        expected_bytes
    );
    let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    let mut statement = db
        .prepare("SELECT result_payload FROM tool_operation_facts WHERE result_payload IS NOT NULL")
        .unwrap();
    let payloads = statement
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(payloads.len(), 8);
    let expected_payload = serde_json::to_vec(&result).unwrap();
    assert!(payloads.iter().all(|payload| payload == &expected_payload));
}

#[tokio::test(flavor = "current_thread")]
async fn counter_sqlite_overflow_rolls_back_completed_response() {
    for cancel in [false, true] {
        let root = TestRoot::new("counter-overflow");
        let store = SessionStore::open_at(root.path()).unwrap();
        let (session, run, operation) = prepared(&store).await;
        store
            .mark_provider_dispatched(run, operation)
            .await
            .unwrap();
        if cancel {
            store
                .cancel_run(MutationRequestId::from_bytes([0x83; 16]), session, run)
                .await
                .unwrap();
        }
        let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        // Seed the projection at its representable boundary without fabricating receipts.
        db.execute("UPDATE runs SET provider_turns = ?1", [i64::MAX])
            .unwrap();
        let snapshot = || {
            [
                "runs",
                "provider_operation_facts",
                "session_entries",
                "logical_sequences",
                "run_state_facts",
                "run_audit_facts",
            ]
            .map(|table| {
                let mut statement = db.prepare(&format!("SELECT * FROM {table}")).unwrap();
                let columns = statement.column_count();
                statement
                    .query_map([], |row| {
                        (0..columns)
                            .map(|column| row.get::<_, rusqlite::types::Value>(column))
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap()
            })
        };
        let before = snapshot();
        let error = store
            .complete_run_success(
                run,
                operation,
                CompletedAssistant {
                    text: "must not commit".into(),
                    refusal: false,
                    provider_response_id: "resp_overflow".into(),
                    usage: usage(),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, PersistenceError::InvalidState { .. }));
        assert_eq!(snapshot(), before);
        assert_eq!(
            store
                .get_run(session, run)
                .await
                .unwrap()
                .unwrap()
                .provider_turns,
            i64::MAX as u64
        );
    }
}

fn usage() -> ProviderUsage {
    ProviderUsage {
        input_tokens: 10,
        cached_input_tokens: 0,
        cache_write_input_tokens: 0,
        output_tokens: 2,
        reasoning_output_tokens: 0,
        total_tokens: 12,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn run_counters_rebuild_only_completed_provider_receipts() {
    for dispatched in [false, true] {
        let root = TestRoot::new("counter-interrupted");
        let store = SessionStore::open_at(root.path()).unwrap();
        let (session, run, operation) = prepared(&store).await;
        if dispatched {
            store
                .mark_provider_dispatched(run, operation)
                .await
                .unwrap();
        }
        assert_eq!(
            store
                .get_run(session, run)
                .await
                .unwrap()
                .unwrap()
                .provider_turns,
            0
        );
        drop(store);
        let store = SessionStore::open_at(root.path()).unwrap();
        let recovered = store.get_run(session, run).await.unwrap().unwrap();
        assert_eq!(recovered.state, RunState::Interrupted);
        assert_eq!(recovered.provider_turns, 0);
        let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        let facts: (i64, i64, i64) = db.query_row(
            "SELECT SUM(fact_kind=1), SUM(fact_kind=2), SUM(fact_kind=3) FROM provider_operation_facts",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert_eq!(facts, (1, i64::from(dispatched), 0));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn run_counters_include_completed_response_discarded_after_cancellation() {
    for cancel in [false, true] {
        let root = TestRoot::new("counter-complete");
        let store = SessionStore::open_at(root.path()).unwrap();
        let (session, run, operation) = prepared(&store).await;
        store
            .mark_provider_dispatched(run, operation)
            .await
            .unwrap();
        if cancel {
            store
                .cancel_run(MutationRequestId::from_bytes([0x83; 16]), session, run)
                .await
                .unwrap();
        }
        let completed = store
            .complete_run_success(
                run,
                operation,
                CompletedAssistant {
                    text: "fixture answer".into(),
                    refusal: false,
                    provider_response_id: "resp_counter".into(),
                    usage: usage(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            completed.state,
            if cancel {
                RunState::Cancelled
            } else {
                RunState::Succeeded
            }
        );
        assert_eq!(completed.provider_turns, 1);
        drop(store);
        let store = SessionStore::open_at(root.path()).unwrap();
        let reopened = store.get_run(session, run).await.unwrap().unwrap();
        assert_eq!(reopened.state, completed.state);
        assert_eq!(reopened.provider_turns, 1);
        let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM provider_operation_facts WHERE fact_kind=3",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM session_entries WHERE entry_kind=2",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            i64::from(!cancel)
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn run_counters_rebuild_direct_mutation_capable_calls_without_execution() {
    let path = || ToolPath::parse("fixture.txt").unwrap();
    let inputs = [
        ToolInput::Read {
            path: path(),
            offset: 1,
            limit: 1,
        },
        ToolInput::Write {
            path: path(),
            content: "fixture".into(),
        },
        ToolInput::Edit {
            path: path(),
            replacements: vec![TextReplacement {
                old_text: "old".into(),
                new_text: "new".into(),
            }],
        },
        ToolInput::Bash {
            command: "printf fixture".into(),
        },
        ToolInput::WebSearch {
            query: "fixture".into(),
        },
        ToolInput::Ipython {
            cell: "1 + 1".into(),
        },
        ToolInput::Task {
            context: "fixture".into(),
            tasks: vec![SubagentTask {
                name: None,
                task: "fixture".into(),
            }],
        },
    ];
    for input in inputs {
        let mutations = u64::from(input.kind().is_mutation());
        let root = TestRoot::new("counter-tools");
        let store = SessionStore::open_at(root.path()).unwrap();
        let (session, run, operation) = prepared(&store).await;
        store
            .mark_provider_dispatched(run, operation)
            .await
            .unwrap();
        store
            .complete_provider_tool_turn(
                run,
                operation,
                CompletedToolTurn {
                    provider_response_id: "resp_counter_tools".into(),
                    usage: usage(),
                    commentary: None,
                    calls: vec![ValidatedProviderCall {
                        provider_call_id: "call_counter".into(),
                        input,
                        opaque_continuation: None,
                    }],
                },
            )
            .await
            .unwrap();
        let live = store.get_run(session, run).await.unwrap().unwrap();
        assert_eq!(
            (live.provider_turns, live.tool_calls, live.tool_mutations),
            (1, 1, mutations)
        );
        drop(store);
        let store = SessionStore::open_at(root.path()).unwrap();
        let reopened = store.get_run(session, run).await.unwrap().unwrap();
        assert!(reopened.state.is_terminal());
        assert_eq!(
            (
                reopened.provider_turns,
                reopened.tool_calls,
                reopened.tool_mutations
            ),
            (1, 1, mutations)
        );
        let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM tool_operation_facts WHERE fact_kind=2",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
}
