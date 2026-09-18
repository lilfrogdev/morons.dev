use super::*;

#[tokio::test]
async fn prepared_search_rechecks_policy_before_http() {
    prepared_search_rechecks_before_http(false, false).await;
}

#[tokio::test]
async fn prepared_search_rechecks_cancellation_before_admission() {
    prepared_search_rechecks_before_http(true, false).await;
}

#[tokio::test]
async fn admitted_search_cancellation_consumes_admission_without_http_or_replay() {
    prepared_search_rechecks_before_http(true, true).await;
}

async fn prepared_search_rechecks_before_http(cancel: bool, admitted: bool) {
    timeout(Duration::from_secs(120), async {
        for native in [false, true] {
            for task in [false, true] {
                let root = TestRoot::new("prepared-web-admission");
                let store = Arc::new(SessionStore::open_for_test(root.path()).unwrap());
                if native {
                    install(&store, 0).await;
                }
                let (session, run, call) = prepare_with_session(&store, native, task, None).await;
                store
                    .mark_tool_dispatched(run, call.call_id, call.operation_id)
                    .await
                    .unwrap();
                let binding = store.web_binding(run, call.call_id).await.unwrap();
                let execution_binding = binding.clone();
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let endpoint = format!("http://{}/search", listener.local_addr().unwrap());
                let barrier = Arc::new(tokio::sync::Barrier::new(2));
                let executor = WebSearchToolExecutor::for_test(store.clone(), endpoint.clone())
                    .with_exa_test_endpoint(endpoint.clone());
                let executor = if admitted {
                    executor.with_admitted_barrier(barrier.clone())
                } else {
                    executor.with_prepared_barrier(barrier.clone())
                };
                let (handle, cancellation) = crate::provider::provider_cancellation();
                let mut execution = tokio::spawn(async move {
                    executor
                        .execute(
                            &ToolInput::WebSearch {
                                query: "public query".into(),
                            },
                            &execution_binding,
                            u16::from(task),
                            u64::from(task),
                            &cancellation,
                        )
                        .await
                });
                tokio::select! {
                    reached = timeout(Duration::from_secs(5), barrier.wait()) => {
                        assert!(reached.is_ok(), "native={native}, task={task}: preparation timed out");
                    }
                    result = &mut execution => panic!("native={native}, task={task}: ended before admission: {result:?}"),
                }
                if cancel {
                    handle.cancel();
                } else {
                    store
                        .set_data_use_policy(
                            MutationRequestId::from_bytes([0x96; 16]),
                            0,
                            DataUseRestrictions {
                                block_training_use: true,
                                require_zero_retention: true,
                            },
                        )
                        .await
                        .unwrap();
                }
                let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
                let sequence = || {
                    db.query_row("SELECT next_value FROM logical_sequences", [], |r| {
                        r.get::<_, i64>(0)
                    })
                    .unwrap()
                };
                let before = sequence();
                barrier.wait().await;
                let result = timeout(Duration::from_secs(5), execution)
                    .await
                    .unwrap_or_else(|_| panic!("native={native}, task={task}: admission timed out"))
                    .unwrap()
                    .unwrap();
                if admitted && native {
                    assert!(matches!(result.error_kind(), Some(ToolErrorKind::WebSearchUncertain(_))));
                } else {
                    assert_eq!(result.error_kind(), Some(if cancel {
                        ToolErrorKind::Cancelled
                    } else {
                        ToolErrorKind::DataUseRestricted
                    }));
                }
                assert_eq!(sequence(), before);
                assert_eq!(
                    db.query_row("SELECT COUNT(*) FROM web_search_attempts", [], |r| r
                        .get::<_, i64>(0))
                        .unwrap(),
                    i64::from(admitted)
                );
                assert_eq!(
                    db.query_row(
                        "SELECT admission_count FROM web_model_bindings WHERE call_id=?1",
                        [call.call_id.as_bytes()],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap(),
                    i64::from(admitted)
                );
                assert_eq!(
                    db.query_row("SELECT COUNT(*) FROM web_search_successes", [], |r| r.get::<_, i64>(0)).unwrap(),
                    0
                );
                if admitted {
                    let executor = WebSearchToolExecutor::for_test(store.clone(), endpoint.clone())
                        .with_exa_test_endpoint(endpoint.clone());
                    let (_, fresh) = crate::provider::provider_cancellation();
                    assert!(executor.execute(&ToolInput::WebSearch { query: "public query".into() }, &binding, u16::from(task), u64::from(task), &fresh).await.is_err());
                    assert_eq!(sequence(), before);
                }
                assert!(
                    timeout(Duration::from_millis(30), listener.accept())
                        .await
                        .is_err()
                );
                drop(db);
                drop(store);
                let reopened = Arc::new(SessionStore::open_for_test(root.path()).unwrap());
                if admitted {
                    assert_eq!(reopened.get_run(session, run).await.unwrap().unwrap().state, RunState::Uncertain);
                    let executor = WebSearchToolExecutor::for_test(reopened.clone(), endpoint.clone())
                        .with_exa_test_endpoint(endpoint);
                    let (_, fresh) = crate::provider::provider_cancellation();
                    assert_eq!(executor.execute(&ToolInput::WebSearch { query: "public query".into() }, &binding, u16::from(task), u64::from(task), &fresh).await.unwrap().error_kind(), Some(ToolErrorKind::Cancelled));
                    assert!(timeout(Duration::from_millis(30), listener.accept()).await.is_err());
                }
                drop(reopened);
                drop(SessionStore::open_for_test(root.path()).unwrap());
            }
        }
    })
    .await
    .unwrap();
}
