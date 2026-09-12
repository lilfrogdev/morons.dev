use super::*;

#[tokio::test(flavor = "current_thread")]
async fn native_long_first_run_compacts_batches_preserves_intent_and_reopens() {
    run_case(6, 12_000, 4, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_same_run_usage_allows_two_large_reads_without_compaction() {
    run_case(2, 1_000, 0, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_uncertain_within_run_compaction_stops_without_retry_and_reopens() {
    run_case(6, 12_000, 0, true).await;
}

async fn run_case(read_count: u32, input_tokens: u64, compactions: u32, fail_summary: bool) {
    const PROMPT: &str = "ORIGINAL-INTENT-ONLY: read the owned fixture as instructed, then finish.";
    let root = TestRoot::new("context-continuation");
    let selected = TestRoot::new("context-continuation-selected");
    fs::write(selected.path().join("step.txt"), "x".repeat(48_000)).unwrap();
    let store = SessionStore::open_for_test(root.path()).unwrap();
    store
        .set_openai_credential(
            PersistenceMutationRequestId::from_bytes([0xe1; 16]),
            0,
            synthetic_tokens(),
        )
        .await
        .unwrap();
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0xe2; 16]),
            None,
            selected.path().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (done, mut stopped) = oneshot::channel::<()>();
    let peer = tokio::spawn(async move {
        let mut roots = 0;
        let mut summaries = 0;
        let mut just_compacted = false;
        for request_index in 0..12 {
            let (mut stream, _) = tokio::select! { result = listener.accept() => result.unwrap(), _ = &mut stopped => break };
            let request = String::from_utf8(read_http_request(&mut stream).await).unwrap();
            let body: serde_json::Value =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(body["model"], "gpt-6-astra");
            let coding = body["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty());
            let output = if coding {
                // Summaries deliberately do not contain this text. Every rebuilt
                // request must still include the original canonical user intent.
                assert!(body["input"].to_string().contains(PROMPT));
                let input = body["input"].as_array().unwrap();
                let calls: std::collections::BTreeSet<_> = input
                    .iter()
                    .filter(|item| item["type"] == "function_call")
                    .map(|item| item["call_id"].as_str().unwrap())
                    .collect();
                let results: std::collections::BTreeSet<_> = input
                    .iter()
                    .filter(|item| item["type"] == "function_call_output")
                    .map(|item| item["call_id"].as_str().unwrap())
                    .collect();
                assert_eq!(calls, results);
                let reasoning = input.iter().any(|item| item["type"] == "reasoning");
                assert_eq!(reasoning, roots > 0 && !just_compacted);
                just_compacted = false;
                roots += 1;
                if roots <= read_count {
                    format!(
                        "{},{}",
                        serde_json::json!({"id":format!("rs_{roots}"),"type":"reasoning","summary":[{"type":"summary_text","text":"synthetic summary"}],"encrypted_content":"synthetic-context-cipher"}),
                        serde_json::json!({"id":format!("fc_{roots}"),"type":"function_call","status":"completed","call_id":format!("call_{roots}"),"name":"read","arguments":serde_json::json!({"path":"step.txt","offset":1,"limit":2}).to_string()})
                    )
                } else {
                    message("FINISHED")
                }
            } else {
                summaries += 1;
                just_compacted = true;
                assert!(!body.to_string().contains("synthetic-context-cipher"));
                assert!(summaries <= 4);
                if fail_summary {
                    assert_eq!(summaries, 1, "uncertain summary must not be retried");
                    write_provider_headers(&mut stream, 3).await;
                    stream.write_all(b"bad").await.unwrap();
                    stream.shutdown().await.unwrap();
                    continue;
                }
                message(
                    "SUMMARY: Earlier fixture reads completed. Continue with the retained intent and recent tool results.",
                )
            };
            let response = provider_output_body(&format!("resp_{request_index}"), &output)
                .replace("muse-spark-1.2", "gpt-6-astra")
                .replace(
                    "\"input_tokens\":8",
                    &format!("\"input_tokens\":{input_tokens}"),
                )
                .replace(
                    "\"cached_tokens\":0",
                    &format!("\"cached_tokens\":{}", input_tokens * 2 / 3),
                )
                .replace(
                    "\"total_tokens\":11",
                    &format!("\"total_tokens\":{}", input_tokens + 3),
                );
            write_provider_headers(&mut stream, response.len()).await;
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
        }
        (roots, summaries)
    });
    let app = ServerApplication::from_native_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.id.as_bytes());
    let accepted = app
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0xe3; 16]),
            session_id,
            text: PROMPT.into(),
            attachments: Vec::new(),
            service: ModelService::OpenAiChatGpt,
            model_id: "gpt-6-astra".into(),
        })
        .await
        .unwrap();
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        accepted
    else {
        panic!("run required")
    };
    let state = wait_for_terminal(&app, session_id, run.id).await;
    let status = app
        .execute_for_local_owner(ApplicationRequest::GetSessionContext {
            session_id,
            service: ModelService::OpenAiChatGpt,
            model_id: "gpt-6-astra".into(),
        })
        .await
        .unwrap();
    let ApplicationOutcome::Response(ApplicationResponse::SessionContextFound { context }) = status
    else {
        panic!("status required")
    };
    app.shutdown().await;
    drop(app);
    let _ = done.send(());
    let (roots, summaries) = peer.await.unwrap();
    let expected_roots = if fail_summary { 2 } else { read_count + 1 };
    let expected_reads = if fail_summary { 2 } else { read_count };
    assert_eq!(
        state,
        if fail_summary {
            RunState::Failed
        } else {
            RunState::Succeeded
        }
    );
    assert_eq!(
        (roots, summaries),
        (expected_roots, if fail_summary { 1 } else { compactions })
    );
    assert!(context.usage_admission);
    assert_eq!(context.maximum_source_bytes, Some(1024 * 1024));
    assert_eq!(context.completed_compactions, u64::from(compactions));
    // Fresh store only, after shutdown. Reopen validates all source/checkpoint,
    // operation and execution-policy facts without replaying any request.
    let reopened = SessionStore::open_for_test(root.path()).unwrap();
    drop(reopened);
    let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    let original: String = db
        .query_row(
            "SELECT text FROM session_entries WHERE entry_kind=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(original, PROMPT);
    let reads: u32 = db
        .query_row("SELECT COUNT(*) FROM tool_calls", [], |r| r.get(0))
        .unwrap();
    assert_eq!(reads, expected_reads);
    let calls: u32 = db
        .query_row(
            "SELECT COUNT(*) FROM provider_operation_facts WHERE fact_kind=2",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(calls, expected_roots);
    if fail_summary {
        assert_eq!(
            db.query_row("SELECT state FROM compaction_operations", [], |r| r
                .get::<_, u32>(0))
                .unwrap(),
            5
        );
    }
    drop(db);
    // Corrupt policy provenance must fail closed on reopen, not reinterpret runs.
    let corrupt = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    corrupt
        .execute(
            "UPDATE context_accounting_epoch SET first_sequence = 9223372036854775807",
            [],
        )
        .unwrap();
    drop(corrupt);
    assert!(SessionStore::open_for_test(root.path()).is_err());
}

fn message(text: &str) -> String {
    serde_json::json!({"id":"msg","type":"message","role":"assistant","status":"completed","phase":"final_answer","content":[{"type":"output_text","text":text,"annotations":[]}]}).to_string()
}
