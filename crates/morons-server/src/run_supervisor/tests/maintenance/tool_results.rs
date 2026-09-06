use super::*;

#[tokio::test(flavor = "current_thread")]
async fn large_tool_results_prepare_background_before_the_next_run_hits_capacity() {
    let (root, selected, store, session) = hardening::fixture("maintenance-tool-results").await;
    drop(store);
    let store = SessionStore::open_for_test_with_maintenance(root.path()).unwrap();
    fs::write(selected.path().join("large.txt"), "x".repeat(20_800)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (sent, ready) = oneshot::channel();
    let peer = tokio::spawn(async move {
        let message = |text: String| {
            serde_json::json!({"id":"msg", "type":"message", "role":"assistant", "status":"completed", "content":[{"type":"output_text", "text":text, "annotations":[]}]}).to_string()
        };
        for index in 1..=4 {
            let (mut call, _) = listener.accept().await.unwrap();
            read_http_request(&mut call).await;
            let output = serde_json::json!({"type":"function_call", "id":format!("fc_{index}"), "call_id":format!("call_{index}"), "name":"read", "arguments":r#"{"path":"large.txt","offset":1,"limit":100}"#, "status":"completed"});
            write_provider_output(&mut call, "read_response", &output.to_string()).await;
            let (mut answer, _) = listener.accept().await.unwrap();
            read_http_request(&mut answer).await;
            write_provider_output(&mut answer, "answer_response", &message("done".to_owned()))
                .await;
        }
        let (mut background, _) = listener.accept().await.unwrap();
        let request = String::from_utf8(read_http_request(&mut background).await).unwrap();
        assert!(
            request_body(&request)["tools"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        write_provider_output(
            &mut background,
            "summary_response",
            &message("TOOL_RESULT_SUMMARY".to_owned()),
        )
        .await;
        sent.send(()).unwrap();
        let (mut next, _) = listener.accept().await.unwrap();
        let request = String::from_utf8(read_http_request(&mut next).await).unwrap();
        assert!(
            request_body(&request)["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty()),
            "a ready summary must not immediately trigger foreground compaction"
        );
        assert!(request.contains("TOOL_RESULT_SUMMARY"));
        write_provider_output(&mut next, "next_response", &message("done".to_owned())).await;
    });
    let application = ServerApplication::from_session_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.as_bytes());
    for index in 1..=4 {
        let run = submit(
            &application,
            session_id,
            format!("Read large.txt once, round {index}"),
            index,
        )
        .await;
        assert_eq!(
            wait_for_terminal(&application, session_id, run.id).await,
            RunState::Succeeded
        );
    }
    time::timeout(TERMINAL_RUN_TEST_TIMEOUT, ready)
        .await
        .unwrap()
        .unwrap();
    time::timeout(TERMINAL_RUN_TEST_TIMEOUT, async {
        loop {
            let ApplicationOutcome::Response(ApplicationResponse::SessionContextFound { context }) =
                application
                    .execute_for_local_owner(ApplicationRequest::GetSessionContext {
                        session_id,
                        service: OpenCodeService::Zen,
                        model_id: "muse-spark-1.2".to_owned(),
                    })
                    .await
                    .unwrap()
            else {
                panic!("context required")
            };
            if context
                .background_compaction
                .latest
                .is_some_and(|job| job.state == morons_protocol::BackgroundCompactionState::Ready)
            {
                assert!(context.conservative_input_tokens > 90_000);
                assert!(context.conservative_input_tokens < 96_000);
                break;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let run = submit(
        &application,
        session_id,
        "Continue without tools".to_owned(),
        5,
    )
    .await;
    assert_eq!(
        wait_for_terminal(&application, session_id, run.id).await,
        RunState::Succeeded
    );
    peer.await.unwrap();
    let ApplicationOutcome::Response(ApplicationResponse::SessionContextFound { context }) =
        application
            .execute_for_local_owner(ApplicationRequest::GetSessionContext {
                session_id,
                service: OpenCodeService::Zen,
                model_id: "muse-spark-1.2".to_owned(),
            })
            .await
            .unwrap()
    else {
        panic!("context required")
    };
    assert_eq!(context.completed_compactions, 0);
    assert_eq!(
        context.background_compaction.latest.unwrap().state,
        morons_protocol::BackgroundCompactionState::Installed
    );
    application.shutdown().await;
    drop(application);
    SessionStore::open_for_test(root.path()).unwrap();
}

async fn submit(
    application: &ServerApplication,
    session_id: SessionId,
    text: String,
    index: u8,
) -> morons_protocol::RunSummary {
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        application
            .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
                mutation_request_id: MutationRequestId::from_bytes([index; 16]),
                session_id,
                text,
                attachments: Vec::new(),
                service: OpenCodeService::Zen,
                model_id: "muse-spark-1.2".to_owned(),
            })
            .await
            .unwrap()
    else {
        panic!("run required")
    };
    run
}
