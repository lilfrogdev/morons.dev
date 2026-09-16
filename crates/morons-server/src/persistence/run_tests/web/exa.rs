use super::*;

mod completion;
mod convergence;
mod interleavings;
use crate::tools::{ToolOutput, ToolResult};
use tokio::{
    io::AsyncWriteExt as _,
    net::TcpListener,
    time::{Duration, timeout},
};

#[tokio::test]
async fn missing_login_routes_root_and_child_to_keyless_exa_without_openai_dispatch() {
    for task in [false, true] {
        let root = TestRoot::new("exa-routing");
        let store = Arc::new(SessionStore::open_for_test(root.path()).unwrap());
        let (run, call) = prepare(&store, false, task).await;
        store
            .mark_tool_dispatched(run, call.call_id, call.operation_id)
            .await
            .unwrap();
        let binding = store.web_binding(run, call.call_id).await.unwrap();
        let openai = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let exa = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let executor = WebSearchToolExecutor::for_test(
            store.clone(),
            format!("http://{}/search", openai.local_addr().unwrap()),
        )
        .with_exa_test_endpoint(format!("http://{}/mcp", exa.local_addr().unwrap()));
        let peer = tokio::spawn(async move {
            let (mut stream, _) = timeout(Duration::from_secs(5), exa.accept())
                .await
                .unwrap()
                .unwrap();
            timeout(
                Duration::from_secs(5),
                crate::provider::openai_auth::tests::mock_request(&mut stream),
            )
            .await
            .unwrap();
            let body = r#"{"jsonrpc":"2.0","id":1,"result":{"structuredContent":{"results":[{"title":"Example","url":"https://example.com","text":"Excerpt"}]}}}"#;
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            exa
        });
        let (_, cancel) = crate::provider::provider_cancellation();
        let input = ToolInput::WebSearch {
            query: "public query".into(),
        };
        let result = executor
            .execute(&input, &binding, u16::from(task), u64::from(task), &cancel)
            .await
            .unwrap();
        assert!(
            matches!(
                result,
                ToolResult::Ok {
                    output: ToolOutput::ExaWeb { .. }
                }
            ),
            "task={task}: {result:?}"
        );
        let exa = peer.await.unwrap();
        let duplicate = executor
            .execute(&input, &binding, u16::from(task), u64::from(task), &cancel)
            .await;
        assert!(matches!(
            duplicate,
            Err(PersistenceError::InvalidState { .. })
        ));
        let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        let recorded = db.query_row(
            "SELECT child,ordinal,query_digest,route,contract_revision FROM web_search_attempts WHERE call_id=?1",
            [call.call_id.as_bytes()],
            |r| Ok((r.get::<_, u16>(0)?, r.get::<_, u16>(1)?, r.get::<_, [u8;32]>(2)?, r.get::<_, i64>(3)?, r.get::<_, u16>(4)?)),
        ).unwrap();
        use sha2::{Digest as _, Sha256};
        assert_eq!(
            recorded,
            (
                u16::from(task),
                u16::from(task),
                Sha256::digest(b"public query").into(),
                1,
                1
            )
        );
        assert!(
            timeout(Duration::from_millis(30), openai.accept())
                .await
                .is_err()
        );
        assert!(
            timeout(Duration::from_millis(30), exa.accept())
                .await
                .is_err()
        );
        if !task {
            store
                .complete_tool_result(run, call.call_id, call.operation_id, result)
                .await
                .unwrap();
            drop(executor);
            drop(store);
            assert!(SessionStore::open_for_test(root.path()).is_ok());
        }
    }
}

#[tokio::test]
async fn concurrent_duplicate_admissions_commit_once_for_root_and_child() {
    use crate::persistence::{WebInvocation, WebRoute};
    use sha2::{Digest as _, Sha256};

    for native in [false, true] {
        for task in [false, true] {
            let root = TestRoot::new("web-concurrent-admission");
            let store = SessionStore::open_for_test(root.path()).unwrap();
            if native {
                install(&store, 0).await;
            }
            let (run, call) = prepare(&store, native, task).await;
            store
                .mark_tool_dispatched(run, call.call_id, call.operation_id)
                .await
                .unwrap();
            let binding = store.web_binding(run, call.call_id).await.unwrap();
            let invocation = WebInvocation {
                child: u16::from(task),
                ordinal: u64::from(task),
                query_digest: Sha256::digest(b"public query").into(),
                route: if native {
                    WebRoute::OpenAi
                } else {
                    WebRoute::ExaMissingCredential
                },
            };
            let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
            let next_sequence = || {
                db.query_row(
                    "SELECT next_value FROM logical_sequences WHERE singleton=1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap()
            };
            let before = next_sequence();
            let (first, second) = tokio::join!(
                store.dispatch_web_search(&binding, invocation.clone()),
                store.dispatch_web_search(&binding, invocation.clone()),
            );
            let results = [first, second];
            assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
            assert_eq!(
                results
                    .iter()
                    .filter(|result| matches!(result, Err(PersistenceError::InvalidState { .. })))
                    .count(),
                1
            );
            assert_eq!(next_sequence(), before + 1);
            assert_eq!(
                db.query_row(
                    "SELECT COUNT(*) FROM web_search_attempts WHERE call_id=?1",
                    [call.call_id.as_bytes()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
                1
            );
            drop(store);
            let reopened = SessionStore::open_for_test(root.path()).unwrap();
            let recovered_sequence = next_sequence();
            assert!(
                reopened
                    .dispatch_web_search(&binding, invocation)
                    .await
                    .is_err()
            );
            assert_eq!(next_sequence(), recovered_sequence);
        }
    }
}

#[tokio::test]
async fn durable_route_admission_rejects_tampering_and_invalid_scope() {
    use crate::persistence::{WebInvocation, WebRoute};
    use sha2::{Digest as _, Sha256};
    for native in [false, true] {
        let root = TestRoot::new("web-admission-integrity");
        let store = SessionStore::open_for_test(root.path()).unwrap();
        if native {
            install(&store, 0).await;
        }
        let (run, call) = prepare(&store, native, false).await;
        store
            .mark_tool_dispatched(run, call.call_id, call.operation_id)
            .await
            .unwrap();
        let binding = store.web_binding(run, call.call_id).await.unwrap();
        let invocation = WebInvocation {
            child: 0,
            ordinal: 0,
            query_digest: Sha256::digest(b"public query").into(),
            route: if native {
                WebRoute::OpenAi
            } else {
                WebRoute::ExaMissingCredential
            },
        };
        assert!(
            store
                .dispatch_web_search(
                    &binding,
                    WebInvocation {
                        child: 1,
                        ..invocation.clone()
                    }
                )
                .await
                .is_err()
        );
        assert!(
            store
                .dispatch_web_search(
                    &binding,
                    WebInvocation {
                        query_digest: [0; 32],
                        ..invocation.clone()
                    }
                )
                .await
                .is_err()
        );
        for route in [
            WebRoute::ExaPolicyDenied,
            if native {
                WebRoute::ExaMissingCredential
            } else {
                WebRoute::OpenAi
            },
        ] {
            assert!(
                store
                    .dispatch_web_search(
                        &binding,
                        WebInvocation {
                            route,
                            ..invocation.clone()
                        }
                    )
                    .await
                    .is_err()
            );
        }
        store
            .dispatch_web_search(&binding, invocation.clone())
            .await
            .unwrap();
        assert!(
            store
                .dispatch_web_search(&binding, invocation)
                .await
                .is_err()
        );
        let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        assert_eq!(
            db.query_row("SELECT route FROM web_search_attempts", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            if native { 0 } else { 1 }
        );
        drop(store);
        let reopened = SessionStore::open_for_test(root.path()).unwrap();
        assert!(
            reopened
                .dispatch_web_search(
                    &binding,
                    WebInvocation {
                        child: 0,
                        ordinal: 0,
                        query_digest: Sha256::digest(b"public query").into(),
                        route: WebRoute::ExaMissingCredential,
                    }
                )
                .await
                .is_err()
        );
        drop(reopened);
        db.execute(
            "UPDATE web_search_attempts SET query_digest=?1",
            [[0_u8; 32]],
        )
        .unwrap();
        assert!(SessionStore::open_for_test(root.path()).is_err());
    }
}

#[tokio::test]
async fn failed_admission_commit_does_not_dispatch_exa() {
    let root = TestRoot::new("exa-admission-failure");
    let store = Arc::new(SessionStore::open_for_test(root.path()).unwrap());
    let (run, call) = prepare(&store, false, false).await;
    store
        .mark_tool_dispatched(run, call.call_id, call.operation_id)
        .await
        .unwrap();
    let binding = store.web_binding(run, call.call_id).await.unwrap();
    let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_web_admission BEFORE INSERT ON web_search_attempts BEGIN SELECT RAISE(ABORT, 'test admission failure'); END;").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/search", listener.local_addr().unwrap());
    let executor = WebSearchToolExecutor::for_test(store.clone(), endpoint.clone())
        .with_exa_test_endpoint(endpoint);
    let (_, cancel) = crate::provider::provider_cancellation();
    assert!(
        executor
            .execute(&call.input, &binding, 0, 0, &cancel)
            .await
            .is_err()
    );
    assert!(
        timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM web_search_attempts", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    db.execute_batch("DROP TRIGGER reject_web_admission;")
        .unwrap();
}

#[tokio::test]
async fn current_privacy_restrictions_block_exa_before_dispatch() {
    for (training, retention) in [(true, false), (false, true), (true, true)] {
        let root = TestRoot::new("exa-policy");
        let store = Arc::new(SessionStore::open_for_test(root.path()).unwrap());
        let (run, call) = prepare(&store, false, false).await;
        store
            .mark_tool_dispatched(run, call.call_id, call.operation_id)
            .await
            .unwrap();
        let binding = store.web_binding(run, call.call_id).await.unwrap();
        store
            .set_data_use_policy(
                MutationRequestId::from_bytes([0x96; 16]),
                0,
                DataUseRestrictions {
                    block_training_use: training,
                    require_zero_retention: retention,
                },
            )
            .await
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/search", listener.local_addr().unwrap());
        let executor = WebSearchToolExecutor::for_test(store.clone(), endpoint.clone())
            .with_exa_test_endpoint(endpoint);
        let (_, cancel) = crate::provider::provider_cancellation();
        let result = executor
            .execute(&call.input, &binding, 0, 0, &cancel)
            .await
            .unwrap();
        assert_eq!(result.error_kind(), Some(ToolErrorKind::DataUseRestricted));
        assert!(
            timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn pre_cancelled_root_and_child_searches_do_not_dispatch() {
    for task in [false, true] {
        let root = TestRoot::new("exa-pre-cancel");
        let store = Arc::new(SessionStore::open_for_test(root.path()).unwrap());
        let (run, call) = prepare(&store, false, task).await;
        store
            .mark_tool_dispatched(run, call.call_id, call.operation_id)
            .await
            .unwrap();
        let binding = store.web_binding(run, call.call_id).await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/search", listener.local_addr().unwrap());
        let executor = WebSearchToolExecutor::for_test(store.clone(), endpoint.clone())
            .with_exa_test_endpoint(endpoint);
        let (handle, cancel) = crate::provider::provider_cancellation();
        handle.cancel();
        let input = ToolInput::WebSearch {
            query: "public query".into(),
        };
        let result = executor
            .execute(&input, &binding, u16::from(task), u64::from(task), &cancel)
            .await
            .unwrap();
        assert_eq!(result.error_kind(), Some(ToolErrorKind::Cancelled));
        assert!(
            timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn changed_bound_generation_does_not_fall_back_for_root_or_child() {
    for task in [false, true] {
        let root = TestRoot::new("exa-generation");
        let store = Arc::new(SessionStore::open_for_test(root.path()).unwrap());
        install(&store, 0).await;
        let (run, call) = prepare(&store, false, task).await;
        store
            .mark_tool_dispatched(run, call.call_id, call.operation_id)
            .await
            .unwrap();
        let binding = store.web_binding(run, call.call_id).await.unwrap();
        assert_eq!(binding.generation, 1);
        assert_eq!(binding.exa_contract_revision, 1);
        install(&store, 1).await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/search", listener.local_addr().unwrap());
        let executor = WebSearchToolExecutor::for_test(store.clone(), endpoint.clone())
            .with_exa_test_endpoint(endpoint);
        let (_, cancel) = crate::provider::provider_cancellation();
        let input = ToolInput::WebSearch {
            query: "public query".into(),
        };
        let result = executor
            .execute(&input, &binding, u16::from(task), u64::from(task), &cancel)
            .await
            .unwrap();
        assert_eq!(
            result.error_kind(),
            Some(ToolErrorKind::WebSearchUnavailable)
        );
        assert_eq!(store.web_binding(run, call.call_id).await.unwrap(), binding);
        assert!(
            timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn native_admission_rechecks_generation_after_preflight() {
    check_native_credential_change_after_preflight(false).await;
}

#[tokio::test]
async fn native_admission_rechecks_removal_after_preflight() {
    check_native_credential_change_after_preflight(true).await;
}

async fn check_native_credential_change_after_preflight(remove: bool) {
    use crate::persistence::{WebInvocation, WebRoute};
    use sha2::{Digest as _, Sha256};

    for task in [false, true] {
        let root = TestRoot::new("web-admission-generation");
        let store = SessionStore::open_for_test(root.path()).unwrap();
        install(&store, 0).await;
        let (run, call) = prepare(&store, false, task).await;
        store
            .mark_tool_dispatched(run, call.call_id, call.operation_id)
            .await
            .unwrap();
        let binding = store.web_binding(run, call.call_id).await.unwrap();
        store.admit_web_binding(&binding).await.unwrap();
        if remove {
            store
                .remove_openai_credential(MutationRequestId::from_bytes([0x97; 16]), 1)
                .await
                .unwrap();
        } else {
            install(&store, 1).await;
        }
        let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        let sequence = || {
            db.query_row(
                "SELECT next_value FROM logical_sequences WHERE singleton=1",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
        };
        let before = sequence();
        let error = store
            .dispatch_web_search(
                &binding,
                WebInvocation {
                    child: u16::from(task),
                    ordinal: u64::from(task),
                    query_digest: Sha256::digest(b"public query").into(),
                    route: WebRoute::OpenAi,
                },
            )
            .await
            .unwrap_err();
        if remove {
            assert!(matches!(
                error,
                PersistenceError::OpenAiCredentialNotConfigured
            ));
        } else {
            assert!(matches!(
                error,
                PersistenceError::CredentialGenerationConflict
            ));
        }
        assert_eq!(sequence(), before);
        let counts: (i64, i64) = db
            .query_row(
                "SELECT admission_count,(SELECT COUNT(*) FROM web_search_attempts WHERE call_id=?1) FROM web_model_bindings WHERE call_id=?1",
                [call.call_id.as_bytes()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(counts, (0, 0));
        drop(db);
        drop(store);
        drop(SessionStore::open_for_test(root.path()).unwrap());
    }
}

#[tokio::test]
async fn dispatch_admission_rechecks_policy_after_preflight() {
    use crate::persistence::{WebInvocation, WebRoute};
    use sha2::{Digest as _, Sha256};

    for native in [false, true] {
        for task in [false, true] {
            let root = TestRoot::new("web-admission-policy");
            let store = SessionStore::open_for_test(root.path()).unwrap();
            if native {
                install(&store, 0).await;
            }
            let (run, call) = prepare(&store, native, task).await;
            store
                .mark_tool_dispatched(run, call.call_id, call.operation_id)
                .await
                .unwrap();
            let binding = store.web_binding(run, call.call_id).await.unwrap();
            if native {
                store.admit_web_binding(&binding).await.unwrap();
            }
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
            let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
            let sequence = || {
                db.query_row(
                    "SELECT next_value FROM logical_sequences WHERE singleton=1",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
            };
            let before = sequence();
            let error = store
                .dispatch_web_search(
                    &binding,
                    WebInvocation {
                        child: u16::from(task),
                        ordinal: u64::from(task),
                        query_digest: Sha256::digest(b"public query").into(),
                        route: if native {
                            WebRoute::OpenAi
                        } else {
                            WebRoute::ExaMissingCredential
                        },
                    },
                )
                .await
                .unwrap_err();
            assert!(matches!(error, PersistenceError::DataUseRestricted));
            assert_eq!(sequence(), before);
            let counts: (i64, i64) = db
                .query_row(
                    "SELECT admission_count,(SELECT COUNT(*) FROM web_search_attempts WHERE call_id=?1) FROM web_model_bindings WHERE call_id=?1",
                    [call.call_id.as_bytes()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(counts, (0, 0));
            drop(db);
            drop(store);
            drop(SessionStore::open_for_test(root.path()).unwrap());
        }
    }
}

#[tokio::test]
async fn policy_change_after_admission_preserves_terminal_evidence() {
    check_change_after_admission(None).await;
}

#[tokio::test]
async fn credential_replacement_after_admission_preserves_terminal_evidence() {
    check_change_after_admission(Some(false)).await;
}

#[tokio::test]
async fn credential_removal_after_admission_preserves_terminal_evidence() {
    check_change_after_admission(Some(true)).await;
}

async fn check_change_after_admission(credential_removal: Option<bool>) {
    use crate::persistence::{WebInvocation, WebRoute};
    use crate::web_diagnostic::{WebCategory, WebFailure, WebStage};
    use sha2::{Digest as _, Sha256};

    for native in [false, true] {
        for task in [false, true] {
            let root = TestRoot::new("web-policy-after-admission");
            let store = SessionStore::open_for_test(root.path()).unwrap();
            if native {
                install(&store, 0).await;
            }
            let (run, call) = prepare(&store, native, task).await;
            store
                .mark_tool_dispatched(run, call.call_id, call.operation_id)
                .await
                .unwrap();
            let binding = store.web_binding(run, call.call_id).await.unwrap();
            let invocation = || WebInvocation {
                child: u16::from(task),
                ordinal: u64::from(task),
                query_digest: Sha256::digest(b"public query").into(),
                route: if native {
                    WebRoute::OpenAi
                } else {
                    WebRoute::ExaMissingCredential
                },
            };
            let admitted_policy = store
                .dispatch_web_search(&binding, invocation())
                .await
                .unwrap();
            if let Some(remove) = credential_removal {
                if remove {
                    // Give the Exa binding a completed login/removal history after admission.
                    if !native {
                        install(&store, 0).await;
                    }
                    store
                        .remove_openai_credential(MutationRequestId::from_bytes([0x97; 16]), 1)
                        .await
                        .unwrap();
                } else {
                    install(&store, u64::from(native)).await;
                }
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
            let evidence = || {
                db.query_row(
                    "SELECT b.admission_count,a.policy_sequence,a.dispatch_sequence,a.admission_digest FROM web_model_bindings b JOIN web_search_attempts a USING(call_id) WHERE b.call_id=?1",
                    [call.call_id.as_bytes()],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, Vec<u8>>(3)?)),
                )
                .unwrap()
            };
            let admitted = evidence();
            assert_eq!(admitted.0, 1);
            assert_eq!(admitted.1, i64::try_from(admitted_policy.sequence).unwrap());
            let sequence = || {
                db.query_row(
                    "SELECT next_value FROM logical_sequences WHERE singleton=1",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
            };
            let before = sequence();
            let redispatch = store.dispatch_web_search(&binding, invocation()).await;
            if credential_removal.is_none() {
                assert!(matches!(
                    redispatch,
                    Err(PersistenceError::DataUseRestricted)
                ));
            } else {
                assert!(redispatch.is_err());
            }
            assert_eq!(sequence(), before);
            store
                .complete_tool_result(
                    run,
                    call.call_id,
                    call.operation_id,
                    ToolResult::error(if native {
                        ToolErrorKind::WebSearchUncertain(WebFailure {
                            stage: WebStage::HttpStatus,
                            category: WebCategory::RequestRejected,
                        })
                    } else {
                        ToolErrorKind::ExaSearchUncertain
                    }),
                )
                .await
                .unwrap();
            assert_eq!(evidence(), admitted);
            drop(store);
            let reopened = SessionStore::open_for_test(root.path()).unwrap();
            assert_eq!(
                reopened.web_binding(run, call.call_id).await.unwrap(),
                binding
            );
            assert_eq!(evidence(), admitted);
            assert_eq!(
                db.query_row(
                    "SELECT COUNT(*) FROM tool_operation_facts WHERE call_id=?1 AND fact_kind=6",
                    [call.call_id.as_bytes()],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap(),
                1
            );
        }
    }
}

#[tokio::test]
async fn exa_http_failure_is_terminal_and_not_replayed_after_reopen() {
    assert_exa_uncertain_not_replayed(false, false).await;
}

#[tokio::test]
async fn exa_post_dispatch_cancellation_is_terminal_and_not_replayed_after_reopen() {
    assert_exa_uncertain_not_replayed(true, false).await;
}

#[tokio::test]
async fn exa_child_post_dispatch_cancellation_is_terminal_and_not_replayed_after_reopen() {
    assert_exa_uncertain_not_replayed(true, true).await;
}

async fn assert_exa_uncertain_not_replayed(cancel_after_request: bool, task: bool) {
    let root = TestRoot::new("exa-uncertain");
    let store = Arc::new(SessionStore::open_for_test(root.path()).unwrap());
    let (run, call) = prepare(&store, false, task).await;
    let input = ToolInput::WebSearch {
        query: "public query".into(),
    };
    let child = u16::from(task);
    let ordinal = u16::from(task);
    let session = store.load_run_context(run).await.unwrap().run.session_id;
    store
        .mark_tool_dispatched(run, call.call_id, call.operation_id)
        .await
        .unwrap();
    let binding = store.web_binding(run, call.call_id).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let executor = WebSearchToolExecutor::for_test(store.clone(), endpoint.clone())
        .with_exa_test_endpoint(endpoint);
    let (handle, cancel) = crate::provider::provider_cancellation();
    let (finished, execution_finished) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let (mut stream, _) = timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        timeout(
            Duration::from_secs(5),
            crate::provider::openai_auth::tests::mock_request(&mut stream),
        )
        .await
        .unwrap();
        if cancel_after_request {
            handle.cancel();
            // Keep the response pending until cancellation completes the executor.
            timeout(Duration::from_secs(5), execution_finished)
                .await
                .unwrap()
                .unwrap();
        } else {
            stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
        }
        listener
    });
    let result = timeout(
        Duration::from_secs(5),
        executor.execute(&input, &binding, child, u64::from(ordinal), &cancel),
    )
    .await
    .unwrap()
    .unwrap();
    if cancel_after_request {
        finished.send(()).unwrap();
    }
    assert_eq!(result.error_kind(), Some(ToolErrorKind::ExaSearchUncertain));
    let listener = peer.await.unwrap();
    store
        .complete_tool_result(run, call.call_id, call.operation_id, result)
        .await
        .unwrap();
    assert_eq!(
        store.get_run(session, run).await.unwrap().unwrap().state,
        RunState::Uncertain
    );
    assert_eq!(
        executor
            .execute(&input, &binding, child, u64::from(ordinal), &cancel)
            .await
            .unwrap()
            .error_kind(),
        Some(ToolErrorKind::Cancelled)
    );
    drop(executor);
    drop(store);
    let reopened = Arc::new(SessionStore::open_for_test(root.path()).unwrap());
    assert_eq!(
        reopened.get_run(session, run).await.unwrap().unwrap().state,
        RunState::Uncertain
    );
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let executor = WebSearchToolExecutor::for_test(reopened, endpoint.clone())
        .with_exa_test_endpoint(endpoint);
    let (_, fresh_cancel) = crate::provider::provider_cancellation();
    assert_eq!(
        executor
            .execute(&input, &binding, child, u64::from(ordinal), &fresh_cancel)
            .await
            .unwrap()
            .error_kind(),
        Some(ToolErrorKind::Cancelled)
    );
    assert!(
        timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn root_terminal_provider_must_match_durable_dispatch() {
    use crate::persistence::{WebInvocation, WebRoute};
    use crate::web_diagnostic::{WebCategory, WebFailure, WebStage};
    use sha2::{Digest as _, Sha256};

    for (native, success) in [(false, false), (true, false), (false, true), (true, true)] {
        let root = TestRoot::new("web-terminal-route");
        let store = SessionStore::open_for_test(root.path()).unwrap();
        if native {
            install(&store, 0).await;
        }
        let (run, call) = prepare(&store, native, false).await;
        store
            .mark_tool_dispatched(run, call.call_id, call.operation_id)
            .await
            .unwrap();
        let binding = store.web_binding(run, call.call_id).await.unwrap();
        store
            .dispatch_web_search(
                &binding,
                WebInvocation {
                    child: 0,
                    ordinal: 0,
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
        let openai = if success {
            successful_web_result(true)
        } else {
            ToolResult::error(ToolErrorKind::WebSearchUncertain(WebFailure {
                stage: WebStage::HttpStatus,
                category: WebCategory::RequestRejected,
            }))
        };
        let exa = if success {
            successful_web_result(false)
        } else {
            ToolResult::error(ToolErrorKind::ExaSearchUncertain)
        };
        let (right, wrong) = if native { (openai, exa) } else { (exa, openai) };
        assert!(
            store
                .complete_tool_result(run, call.call_id, call.operation_id, wrong.clone())
                .await
                .is_err()
        );
        if success {
            record_success(&store, &binding, native, 0, 0).await;
        }
        store
            .complete_tool_result(run, call.call_id, call.operation_id, right)
            .await
            .unwrap();
        drop(store);
        drop(SessionStore::open_for_test(root.path()).unwrap());
        let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        assert_eq!(
            db.execute(
                "UPDATE tool_operation_facts SET result_payload=?1 WHERE call_id=?2 AND fact_kind BETWEEN 3 AND 6",
                rusqlite::params![serde_json::to_vec(&wrong).unwrap(), call.call_id.as_bytes()],
            )
            .unwrap(),
            1
        );
        assert!(SessionStore::open_for_test(root.path()).is_err());
    }
}

#[tokio::test]
async fn success_provenance_corruption_fails_closed_after_reopen() {
    use crate::persistence::{WebInvocation, WebRoute};
    use sha2::{Digest as _, Sha256};

    for native in [false, true] {
        for corruption in [
            "DELETE FROM web_search_successes",
            "UPDATE web_search_successes SET success_digest=zeroblob(32)",
            "UPDATE web_search_successes SET completion_sequence=1",
            "UPDATE web_model_bindings SET success_count=0",
            "UPDATE web_search_attempts SET query_digest=zeroblob(32)",
            "UPDATE web_binding_provenance SET absence_generation=COALESCE(absence_generation,0)+1",
            "UPDATE web_binding_provenance SET evidence_digest=zeroblob(32)",
            "DELETE FROM web_binding_provenance",
        ] {
            let root = TestRoot::new("success-provenance-corruption");
            let store = SessionStore::open_for_test(root.path()).unwrap();
            if native {
                install(&store, 0).await;
            }
            let (run, call) = prepare(&store, native, false).await;
            store
                .mark_tool_dispatched(run, call.call_id, call.operation_id)
                .await
                .unwrap();
            let binding = store.web_binding(run, call.call_id).await.unwrap();
            store
                .dispatch_web_search(
                    &binding,
                    WebInvocation {
                        child: 0,
                        ordinal: 0,
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
            record_success(&store, &binding, native, 0, 0).await;
            store
                .complete_tool_result(
                    run,
                    call.call_id,
                    call.operation_id,
                    successful_web_result(native),
                )
                .await
                .unwrap();
            drop(store);
            drop(SessionStore::open_for_test(root.path()).unwrap());
            let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
            assert_eq!(db.execute(corruption, []).unwrap(), 1);
            drop(db);
            assert!(
                SessionStore::open_for_test(root.path()).is_err(),
                "{corruption}"
            );
        }
    }
}

async fn record_success(
    store: &SessionStore,
    binding: &crate::persistence::WebBinding,
    native: bool,
    child: u16,
    ordinal: u64,
) -> crate::tools::WebSuccess {
    use crate::persistence::{WebInvocation, WebRoute};
    use sha2::{Digest as _, Sha256};
    store
        .complete_web_search(
            binding,
            WebInvocation {
                child,
                ordinal,
                query_digest: Sha256::digest(b"public query").into(),
                route: if native {
                    WebRoute::OpenAi
                } else {
                    WebRoute::ExaMissingCredential
                },
            },
            successful_web_result(native),
        )
        .await
        .unwrap()
}

fn successful_web_result(native: bool) -> ToolResult {
    use crate::tools::{
        ExaWebResult, HostedWebResult, SubagentUsage, WebCitation, WebReceipt, WebSearchResult,
    };
    let output = if native {
        ToolOutput::OpenAiWeb {
            result: HostedWebResult {
                query: "public query".into(),
                answer: "Fixture answer".into(),
                citations: vec![WebCitation {
                    title: "Example".into(),
                    url: "https://example.com".into(),
                }],
                receipt: WebReceipt {
                    model_id: "gpt-5.5".into(),
                    contract_revision: 1,
                    search_calls: 1,
                    open_page_calls: 0,
                    find_in_page_calls: 0,
                    usage: SubagentUsage::default(),
                },
            },
        }
    } else {
        ToolOutput::ExaWeb {
            result: ExaWebResult {
                query: "public query".into(),
                contract_revision: 1,
                results: vec![WebSearchResult {
                    title: "Example".into(),
                    url: "https://example.com".into(),
                    snippet: "Fixture excerpt".into(),
                }],
            },
        }
    };
    let result = ToolResult::Ok { output };
    assert!(crate::tools::validate_canonical_result_for_input(
        &ToolInput::WebSearch {
            query: "public query".into()
        },
        &result,
    ));
    result
}

#[tokio::test]
async fn deleting_search_history_removes_admissions_but_preserves_selected_directory() {
    for native in [false, true] {
        for task in [false, true] {
            check_search_history_deletion(native, task).await;
        }
    }
}

async fn check_search_history_deletion(native: bool, task: bool) {
    use crate::persistence::{WebInvocation, WebRoute};
    use sha2::{Digest as _, Sha256};

    {
        let root = TestRoot::new("web-delete");
        let selected = TestRoot::new("web-delete-selected");
        fs::write(selected.path().join("keep.txt"), "keep").unwrap();
        let store = SessionStore::open_for_test(root.path()).unwrap();
        if native {
            install(&store, 0).await;
        }
        let (run, call) = prepare_at(&store, native, task, Some(selected.path())).await;
        let session = store.load_run_context(run).await.unwrap().run.session_id;
        let other = store
            .create_session_at(
                MutationRequestId::from_bytes([0xd0; 16]),
                None,
                selected.path().to_str().unwrap().into(),
            )
            .await
            .unwrap();
        store
            .mark_tool_dispatched(run, call.call_id, call.operation_id)
            .await
            .unwrap();
        let binding = store.web_binding(run, call.call_id).await.unwrap();
        store
            .dispatch_web_search(
                &binding,
                WebInvocation {
                    child: u16::from(task),
                    ordinal: u64::from(task),
                    query_digest: binding
                        .query_digest
                        .unwrap_or_else(|| Sha256::digest(b"public query").into()),
                    route: if native {
                        WebRoute::OpenAi
                    } else {
                        WebRoute::ExaMissingCredential
                    },
                },
            )
            .await
            .unwrap();
        record_success(&store, &binding, native, u16::from(task), u64::from(task)).await;
        store
            .complete_tool_result(
                run,
                call.call_id,
                call.operation_id,
                if task {
                    ToolResult::error(ToolErrorKind::Cancelled)
                } else {
                    successful_web_result(native)
                },
            )
            .await
            .unwrap();
        store.finish_run_stopped(run, None).await.unwrap();
        let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        let attempts = || {
            db.query_row("SELECT COUNT(*) FROM web_search_attempts", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
        };
        assert_eq!(attempts(), 1);
        drop(store);
        crate::persistence::data_use::tests::restore_schema_35(&db);
        db.execute_batch(include_str!("../../schema_v36.sql"))
            .unwrap();
        let store = SessionStore::open_for_test(root.path()).unwrap();
        assert_eq!(attempts(), 1);
        assert_eq!(
            db.query_row(
                "SELECT admission_count FROM web_model_bindings WHERE call_id=?1",
                [call.call_id.as_bytes()],
                |r| r.get::<_, i64>(0),
            )
            .unwrap(),
            1
        );
        let on_delete: String = db
            .query_row(
                "SELECT on_delete FROM pragma_foreign_key_list('web_search_attempts')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(on_delete, "NO ACTION");
        store
            .set_session_archived(MutationRequestId::from_bytes([0xd3; 16]), session, true)
            .await
            .unwrap();
        store
            .delete_session(MutationRequestId::from_bytes([0xd4; 16]), session)
            .await
            .unwrap();
        assert_eq!(attempts(), 0);
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM web_model_bindings", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            fs::read_to_string(selected.path().join("keep.txt")).unwrap(),
            "keep"
        );
        drop(store);
        let reopened = SessionStore::open_for_test(root.path()).unwrap();
        assert!(reopened.get_session(other.id).await.unwrap().is_some());
        assert!(reopened.get_session(session).await.unwrap().is_none());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn root_terminal_requires_attempt_except_before_migration_epoch() {
    for native in [false, true] {
        check_uncertain_terminal_admissions(native, false).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn task_uncertainty_requires_matching_attempt_except_before_migration_epoch() {
    for native in [false, true] {
        check_uncertain_terminal_admissions(native, true).await;
    }
}

async fn check_uncertain_terminal_admissions(native: bool, task: bool) {
    use crate::persistence::{WebInvocation, WebRoute};
    use crate::web_diagnostic::{WebCategory, WebFailure, WebStage};
    use sha2::{Digest as _, Sha256};

    let root = TestRoot::new("web-missing-attempt");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    if native {
        install(&store, 0).await;
    }
    let (run, call) = prepare(&store, native, task).await;
    store
        .mark_tool_dispatched(run, call.call_id, call.operation_id)
        .await
        .unwrap();
    let binding = store.web_binding(run, call.call_id).await.unwrap();
    let result = ToolResult::error(if native {
        ToolErrorKind::WebSearchUncertain(WebFailure {
            stage: WebStage::HttpStatus,
            category: WebCategory::RequestRejected,
        })
    } else {
        ToolErrorKind::ExaSearchUncertain
    });
    assert!(
        store
            .complete_tool_result(run, call.call_id, call.operation_id, result.clone())
            .await
            .is_err()
    );
    store
        .dispatch_web_search(
            &binding,
            WebInvocation {
                child: u16::from(task),
                ordinal: u64::from(task),
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
    let wrong_provider = ToolResult::error(if native {
        ToolErrorKind::ExaSearchUncertain
    } else {
        ToolErrorKind::WebSearchUncertain(WebFailure {
            stage: WebStage::HttpStatus,
            category: WebCategory::RequestRejected,
        })
    });
    assert!(
        store
            .complete_tool_result(run, call.call_id, call.operation_id, wrong_provider)
            .await
            .is_err()
    );
    store
        .complete_tool_result(run, call.call_id, call.operation_id, result)
        .await
        .unwrap();
    drop(store);
    drop(SessionStore::open_for_test(root.path()).unwrap());
    let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    db.execute("DELETE FROM web_search_attempts", []).unwrap();
    assert!(SessionStore::open_for_test(root.path()).is_err());
    // Recreate the pre-epoch schema to exercise migration of historical terminal facts.
    crate::persistence::data_use::tests::restore_schema_35(&db);
    drop(db);
    drop(SessionStore::open_for_test(root.path()).unwrap());
}

#[tokio::test]
async fn child_search_counts_require_durable_admissions() {
    for native in [false, true] {
        check_child_search_admissions(native).await;
    }
}

async fn check_child_search_admissions(native: bool) {
    use crate::persistence::{WebInvocation, WebRoute};
    use crate::tools::{SubagentResult, SubagentStatus, SubagentUsage};
    use sha2::{Digest as _, Sha256};

    let root = TestRoot::new("child-search-admissions");
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
    let model = store.task_model_binding(run, call.call_id).await.unwrap();
    let child = SubagentResult {
        index: 1,
        name: None,
        status: SubagentStatus::Succeeded,
        model: Some(crate::tools::SubagentModelDisclosure {
            service: model.service.model_service().label().into(),
            model_id: model.model_id,
            protocol_revision: model.protocol_revision,
        }),
        output: "source excerpts".into(),
        provider_turns: 1,
        tool_calls: 1,
        tool_mutations: 0,
        usage: SubagentUsage::default(),
        web_successes: Vec::new(),
        web_searches: if native {
            vec![crate::tools::WebReceipt {
                model_id: "gpt-5.5".into(),
                contract_revision: 1,
                search_calls: 1,
                open_page_calls: 0,
                find_in_page_calls: 0,
                usage: SubagentUsage::default(),
            }]
        } else {
            vec![]
        },
        exa_searches: u64::from(!native),
    };
    let result = |child| ToolResult::Ok {
        output: ToolOutput::Task {
            results: vec![child],
        },
    };
    assert!(
        store
            .complete_tool_result(run, call.call_id, call.operation_id, result(child.clone()))
            .await
            .is_err()
    );
    store
        .dispatch_web_search(
            &binding,
            WebInvocation {
                child: 1,
                ordinal: 2,
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
    assert!(
        store
            .complete_tool_result(run, call.call_id, call.operation_id, result(child.clone()))
            .await
            .is_err()
    );
    let success = record_success(&store, &binding, native, 1, 2).await;
    let child = SubagentResult {
        tool_calls: 2,
        web_successes: vec![success],
        ..child
    };
    for field in 0..8 {
        let mut corrupted = child.clone();
        let reference = &mut corrupted.web_successes[0];
        match field {
            0 => reference.operation_id[0] ^= 1,
            1 => reference.child += 1,
            2 => reference.ordinal -= 1,
            3 => reference.query_digest[0] ^= 1,
            4 => reference.exa = !reference.exa,
            5 => reference.result_digest[0] ^= 1,
            6 => corrupted.web_successes.clear(),
            7 => corrupted
                .web_successes
                .push(corrupted.web_successes[0].clone()),
            _ => unreachable!(),
        }
        assert!(
            store
                .complete_tool_result(run, call.call_id, call.operation_id, result(corrupted))
                .await
                .is_err(),
            "corrupted success reference field {field}"
        );
    }
    let valid_result = result(child);
    store
        .complete_tool_result(run, call.call_id, call.operation_id, valid_result.clone())
        .await
        .unwrap();
    drop(store);
    drop(SessionStore::open_for_test(root.path()).unwrap());
    let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    let mut corrupted = valid_result.clone();
    let ToolResult::Ok {
        output: ToolOutput::Task { results },
    } = &mut corrupted
    else {
        unreachable!()
    };
    results[0].web_successes[0].query_digest[0] ^= 1;
    assert_eq!(
        db.execute(
            "UPDATE tool_operation_facts SET result_payload=?1 WHERE call_id=?2 AND fact_kind=3",
            rusqlite::params![
                serde_json::to_vec(&corrupted).unwrap(),
                call.call_id.as_bytes()
            ],
        )
        .unwrap(),
        1
    );
    assert!(SessionStore::open_for_test(root.path()).is_err());
    db.execute(
        "UPDATE tool_operation_facts SET result_payload=?1 WHERE call_id=?2 AND fact_kind=3",
        rusqlite::params![
            serde_json::to_vec(&valid_result).unwrap(),
            call.call_id.as_bytes()
        ],
    )
    .unwrap();
    drop(SessionStore::open_for_test(root.path()).unwrap());
    db.execute_batch(
        "PRAGMA foreign_keys=OFF; DELETE FROM web_search_attempts; PRAGMA foreign_keys=ON;",
    )
    .unwrap();
    assert!(SessionStore::open_for_test(root.path()).is_err());
    crate::persistence::data_use::tests::restore_schema_35(&db);
    drop(db);
    drop(SessionStore::open_for_test(root.path()).unwrap());
}

#[tokio::test]
async fn deleted_admission_without_terminal_evidence_fails_closed() {
    check_deleted_admission(None).await;
}

#[tokio::test]
async fn deleted_admission_after_generic_error_fails_closed() {
    check_deleted_admission(Some(ToolErrorKind::ResourceLimit)).await;
}

async fn check_deleted_admission(terminal: Option<ToolErrorKind>) {
    use crate::persistence::{WebInvocation, WebRoute};
    use sha2::{Digest as _, Sha256};

    for native in [false, true] {
        for task in [false, true] {
            let root = TestRoot::new("deleted-interrupted-web-admission");
            let store = SessionStore::open_for_test(root.path()).unwrap();
            if native {
                install(&store, 0).await;
            }
            let (run, call) = prepare(&store, native, task).await;
            store
                .mark_tool_dispatched(run, call.call_id, call.operation_id)
                .await
                .unwrap();
            let binding = store.web_binding(run, call.call_id).await.unwrap();
            store
                .dispatch_web_search(
                    &binding,
                    WebInvocation {
                        child: u16::from(task),
                        ordinal: u64::from(task),
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
            if let Some(error) = terminal {
                store
                    .complete_tool_result(
                        run,
                        call.call_id,
                        call.operation_id,
                        ToolResult::error(error),
                    )
                    .await
                    .unwrap();
                store.finish_run_stopped(run, None).await.unwrap();
            }
            drop(store);
            if terminal.is_some() {
                let store = SessionStore::open_for_test(root.path()).unwrap();
                drop(store);
            }
            let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
            let count: i64 = db
                .query_row(
                    "SELECT admission_count FROM web_model_bindings WHERE call_id=?1",
                    [call.call_id.as_bytes()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1);
            db.execute(
                "DELETE FROM web_search_attempts WHERE call_id=?1",
                [call.call_id.as_bytes()],
            )
            .unwrap();
            drop(db);
            assert!(
                SessionStore::open_for_test(root.path()).is_err(),
                "native={native}, task={task}, terminal={terminal:?}"
            );
        }
    }
}
