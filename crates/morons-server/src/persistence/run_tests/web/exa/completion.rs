use super::*;

#[tokio::test]
async fn failed_success_commit_preserves_admission_and_recovers_without_replay() {
    for native in [false, true] {
        for task in [false, true] {
            let root = TestRoot::new("web-success-commit-failure");
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
            let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
            db.execute_batch("CREATE TRIGGER reject_web_success BEFORE INSERT ON web_search_successes BEGIN SELECT RAISE(ABORT, 'test completion failure'); END;").unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/search", listener.local_addr().unwrap());
            let executor = WebSearchToolExecutor::for_test(store.clone(), endpoint.clone())
                .with_exa_test_endpoint(endpoint.clone());
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
                let (content_type, body) = if native {
                    (
                        "text/event-stream",
                        crate::provider::openai_web::response_sources_fixture(),
                    )
                } else {
                    ("application/json", br#"{"jsonrpc":"2.0","id":1,"result":{"structuredContent":{"results":[{"title":"Example","url":"https://example.com","text":"Excerpt"}]}}}"#.to_vec())
                };
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
                stream.write_all(&body).await.unwrap();
                listener
            });
            let input = ToolInput::WebSearch {
                query: "public query".into(),
            };
            let child = u16::from(task);
            let (_, cancel) = crate::provider::provider_cancellation();
            let result = timeout(
                Duration::from_secs(5),
                executor.execute(&input, &binding, child, u64::from(child), &cancel),
            )
            .await
            .unwrap();
            assert!(
                matches!(result, Err(PersistenceError::Sqlite(_))),
                "{result:?}"
            );
            let listener = peer.await.unwrap();
            let counts: (i64, i64, i64, i64) = db.query_row("SELECT admission_count,success_count,(SELECT COUNT(*) FROM web_search_attempts),(SELECT COUNT(*) FROM web_search_successes) FROM web_model_bindings", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
            assert_eq!(counts, (1, 0, 1, 0));
            let sequence: i64 = db
                .query_row("SELECT next_value FROM logical_sequences", [], |r| r.get(0))
                .unwrap();
            db.execute_batch("DROP TRIGGER reject_web_success;")
                .unwrap();
            assert!(
                executor
                    .execute(&input, &binding, child, u64::from(child), &cancel)
                    .await
                    .is_err()
            );
            assert_eq!(
                sequence,
                db.query_row("SELECT next_value FROM logical_sequences", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap()
            );
            drop(executor);
            drop(store);
            let reopened = Arc::new(SessionStore::open_for_test(root.path()).unwrap());
            assert_eq!(
                reopened.get_run(session, run).await.unwrap().unwrap().state,
                RunState::Uncertain
            );
            let executor = WebSearchToolExecutor::for_test(reopened.clone(), endpoint.clone())
                .with_exa_test_endpoint(endpoint);
            assert_eq!(
                executor
                    .execute(&input, &binding, child, u64::from(child), &cancel)
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
            drop(executor);
            drop(reopened);
            drop(SessionStore::open_for_test(root.path()).unwrap());
        }
    }
}
