use super::*;

#[tokio::test(flavor = "current_thread")]
async fn archive_and_delete_drain_background_network_without_touching_selected_files() {
    for delete in [false, true] {
        let (root, selected, store, session, _) = eligible(true).await;
        lower_advisory_usage(&root);
        fs::write(selected.path().join("keep.txt"), "SELECTED_FILE").unwrap();
        fs::write(selected.path().join("AGENTS.md"), "SELECTED_GUIDANCE").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (sent, received) = oneshot::channel();
        let peer = tokio::spawn(async move {
            let output = r#"{"id":"msg","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"done","annotations":[]}]}"#;
            let (mut foreground, _) = listener.accept().await.unwrap();
            read_http_request(&mut foreground).await;
            let body = provider_output_body("root_response", output)
                .replace("\"input_tokens\":8", "\"input_tokens\":60000")
                .replace("\"total_tokens\":11", "\"total_tokens\":60003");
            write_provider_headers(&mut foreground, body.len()).await;
            foreground.write_all(body.as_bytes()).await.unwrap();
            foreground.shutdown().await.unwrap();
            let (mut background, _) = listener.accept().await.unwrap();
            read_http_request(&mut background).await;
            sent.send(()).unwrap();
            let mut byte = [0; 1];
            assert_eq!(
                time::timeout(TERMINAL_RUN_TEST_TIMEOUT, background.read(&mut byte))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
            assert!(
                time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err()
            );
        });
        let application = ServerApplication::from_session_store_for_test(
            Arc::try_unwrap(store).ok().unwrap(),
            &base,
        );
        let session_id = SessionId::from_bytes(*session.as_bytes());
        let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
            application
                .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
                    mutation_request_id: MutationRequestId::from_bytes([6; 16]),
                    session_id,
                    text: "Continue".to_owned(),
                    attachments: Vec::new(),
                    service: OpenCodeService::Zen,
                    model_id: "muse-spark-1.2".to_owned(),
                })
                .await
                .unwrap()
        else {
            panic!("run required")
        };
        assert_eq!(
            wait_for_terminal(&application, session_id, run.id).await,
            RunState::Succeeded
        );
        time::timeout(TERMINAL_RUN_TEST_TIMEOUT, received)
            .await
            .unwrap()
            .unwrap();
        if delete {
            assert!(matches!(
                application
                    .execute_for_local_owner(ApplicationRequest::DeleteSession {
                        mutation_request_id: MutationRequestId::from_bytes([8; 16]),
                        session_id,
                    })
                    .await,
                Err(morons_protocol::ApplicationError::SessionNotArchived)
            ));
        }
        let request = ApplicationRequest::SetSessionArchived {
            mutation_request_id: MutationRequestId::from_bytes([7; 16]),
            session_id,
            archived: true,
        };
        time::timeout(
            TERMINAL_RUN_TEST_TIMEOUT,
            application.execute_for_local_owner(request),
        )
        .await
        .unwrap()
        .unwrap();
        peer.await.unwrap();
        if delete {
            application
                .execute_for_local_owner(ApplicationRequest::DeleteSession {
                    mutation_request_id: MutationRequestId::from_bytes([9; 16]),
                    session_id,
                })
                .await
                .unwrap();
        }
        assert_eq!(
            fs::read_to_string(selected.path().join("keep.txt")).unwrap(),
            "SELECTED_FILE"
        );
        assert_eq!(
            fs::read_to_string(selected.path().join("AGENTS.md")).unwrap(),
            "SELECTED_GUIDANCE"
        );
        let connection =
            rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        if delete {
            assert_eq!(
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM compaction_maintenance_jobs",
                        [],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
        } else {
            assert_eq!(
                connection
                    .query_row("SELECT state FROM compaction_maintenance_jobs", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap(),
                MaintenanceState::Uncertain as i64
            );
            assert!(
                connection
                    .query_row("SELECT archived FROM sessions", [], |row| row
                        .get::<_, bool>(0))
                    .unwrap()
            );
        }
        application.shutdown().await;
    }
}
