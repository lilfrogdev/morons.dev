use super::*;

async fn child_scenario(responses: Vec<String>) -> Vec<String> {
    child_scenario_with_contents(responses, "preserved finding", "preserved finding").await
}

async fn child_scenario_with_contents(
    responses: Vec<String>,
    initial_contents: &str,
    expected: &str,
) -> Vec<String> {
    let root = TestRoot::new("child-recovery");
    let selected = TestRoot::new("child-recovery-selected");
    fs::write(selected.path().join("alpha.txt"), initial_contents).unwrap();
    let store = SessionStore::open_for_test(root.path()).unwrap();
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0x74; 16]),
            0,
            b"test-key".to_vec(),
        )
        .await
        .unwrap();
    store
        .set_subagent_model_setting(
            PersistenceMutationRequestId::from_bytes([0x73; 16]),
            crate::persistence::SubagentModelSetting::Explicit {
                service: RunService::Go,
                model_id: "glm-5.3-flash".to_owned(),
            },
        )
        .await
        .unwrap();
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0x75; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        let (mut parent, _) = listener.accept().await.unwrap();
        read_http_request(&mut parent).await;
        let call = serde_json::json!({"id":"fc_task", "type":"function_call", "status":"completed", "call_id":"task_recovery", "name":"task", "arguments": serde_json::json!({"context":"Read and report", "tasks":[{"name":"check", "task":"Inspect alpha.txt"}]}).to_string()});
        write_provider_output(&mut parent, "resp_task", &call.to_string()).await;
        for body in responses {
            let (mut child, _) = time::timeout(Duration::from_secs(10), listener.accept())
                .await
                .unwrap()
                .unwrap();
            requests.push(String::from_utf8(read_http_request(&mut child).await).unwrap());
            write_provider_headers(&mut child, body.len()).await;
            child.write_all(body.as_bytes()).await.unwrap();
            child.shutdown().await.unwrap();
        }
        let (mut parent, _) = time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .unwrap()
            .unwrap();
        requests.push(String::from_utf8(read_http_request(&mut parent).await).unwrap());
        let final_output = serde_json::json!({"id":"msg_done", "type":"message", "role":"assistant", "status":"completed", "phase":"final_answer", "content":[{"type":"output_text", "text":"done", "annotations":[]}]});
        write_provider_output(&mut parent, "resp_done", &final_output.to_string()).await;
        requests
    });
    let app = ServerApplication::from_session_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.id.as_bytes());
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) = app
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0x76; 16]),
            session_id,
            text: "Delegate a check".to_owned(),
            attachments: Vec::new(),
            service: ModelService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        wait_for_terminal(&app, session_id, run.id).await,
        RunState::Succeeded
    );
    let requests = server.await.unwrap();
    assert_eq!(
        fs::read_to_string(selected.path().join("alpha.txt")).unwrap(),
        expected
    );
    requests
}

fn tool_batch(calls: Vec<(&str, &str, serde_json::Value)>) -> String {
    let calls: Vec<_> = calls
        .into_iter()
        .enumerate()
        .map(|(index, (id, name, arguments))| {
            serde_json::json!({"index": index, "id": id, "type": "function", "function": {
                "name": name, "arguments": arguments.to_string()
            }})
        })
        .collect();
    let chunk = serde_json::json!({
        "id": "chat_batch", "created": 1, "model": "glm-5.3-flash",
        "choices": [{"index": 0, "delta": {"role": "assistant", "tool_calls": calls}, "finish_reason": "tool_calls"}],
        "usage": {"prompt_tokens": 8, "completion_tokens": 3, "total_tokens": 11}
    });
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn write_arguments() -> serde_json::Value {
    serde_json::json!({"path": "alpha.txt", "content": "changed"})
}

#[tokio::test(flavor = "current_thread")]
async fn child_rejects_entire_batch_before_valid_write_when_later_call_is_invalid() {
    let requests = child_scenario(vec![
        tool_batch(vec![
            ("write_rejected", "write", write_arguments()),
            (
                "read_invalid",
                "read",
                serde_json::json!({"path": "alpha.txt", "offset": 0, "limit": 10}),
            ),
        ]),
        chat_text_output_body("chat_final", "nothing executed"),
    ])
    .await;
    assert!(requests[1].contains("read offset or limit"));
    assert!(!requests[1].contains("write_rejected"));
    assert!(!requests[1].contains("read_invalid"));
}

#[tokio::test(flavor = "current_thread")]
async fn child_rejects_entire_batch_with_cross_turn_duplicate() {
    let requests = child_scenario(vec![
        read_response(0, false),
        tool_batch(vec![
            ("write_rejected", "write", write_arguments()),
            (
                "read_0",
                "read",
                serde_json::json!({"path": "alpha.txt", "offset": 1, "limit": 10}),
            ),
        ]),
    ])
    .await;
    assert!(
        requests
            .last()
            .unwrap()
            .contains("reused a provider tool-call identifier")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn child_executes_batch_crossing_former_mutation_limit() {
    let ids: Vec<_> = (0..7).map(|i| format!("write_{i}")).collect();
    let accepted = tool_batch(
        ids.iter()
            .map(|id| (id.as_str(), "write", write_arguments()))
            .collect(),
    );
    let rejected = tool_batch(vec![
        (
            "excess_1",
            "write",
            serde_json::json!({"path": "alpha.txt", "content": "must not execute"}),
        ),
        ("excess_2", "write", write_arguments()),
    ]);
    let requests = child_scenario_with_contents(
        vec![
            accepted,
            rejected,
            chat_text_output_body("chat_final", "retained changes"),
        ],
        "preserved finding",
        "changed",
    )
    .await;
    assert!(!requests[2].contains("REPORT ONLY"));
    assert!(requests[2].contains("excess_1"));
    assert!(requests[3].contains("succeeded"));
    assert!(requests[3].contains("retained changes"));
}

#[tokio::test(flavor = "current_thread")]
async fn child_keeps_tools_after_former_mutation_limit() {
    let ids: Vec<_> = (0..8).map(|i| format!("write_{i}")).collect();
    let accepted = tool_batch(
        ids.iter()
            .map(|id| (id.as_str(), "write", write_arguments()))
            .collect(),
    );
    let requests = child_scenario_with_contents(
        vec![
            accepted,
            chat_text_output_body("chat_final", "finished writes"),
        ],
        "preserved finding",
        "changed",
    )
    .await;
    assert!(!requests[1].contains("REPORT ONLY"));
    assert!(requests[2].contains("succeeded"));
}

#[tokio::test(flavor = "current_thread")]
async fn child_correction_after_former_turn_limit_preserves_findings() {
    let mut responses: Vec<_> = (0..10).map(|i| read_response(i, false)).collect();
    responses.push(read_response(99, true));
    responses.push(chat_text_output_body("chat_final", "prior findings"));
    let requests = child_scenario(responses).await;
    let final_request = &requests[requests.len() - 2];
    assert!(final_request.contains("read offset or limit"));
    assert!(!final_request.contains("REPORT ONLY"));
    assert!(!final_request.contains("read_99"));
    assert_eq!(requests.len(), 13);
}

#[tokio::test(flavor = "current_thread")]
async fn child_compaction_uses_tool_free_summary_and_preserves_recent_result() {
    let contents = format!("preserved finding {}\n", "x".repeat(280)).repeat(200);
    let requests = child_scenario_with_contents(
        vec![
            large_read_response(0),
            large_read_response(1),
            chat_text_output_body("chat_summary", "summary retains the completed read"),
            chat_text_output_body("chat_final", "finished after compaction"),
        ],
        &contents,
        &contents,
    )
    .await;

    assert_eq!(requests.len(), 5);
    let summary: serde_json::Value =
        serde_json::from_str(requests[2].split("\r\n\r\n").nth(1).expect("summary body"))
            .expect("summary request JSON");
    assert!(summary.get("tools").is_none());
    assert!(requests[3].contains("Untrusted lossy child checkpoint"));
    assert!(requests[3].contains("summary retains the completed read"));
    assert!(requests[3].contains("preserved finding"));
    assert!(requests[4].contains("finished after compaction"));
}

#[tokio::test(flavor = "current_thread")]
async fn child_repeated_compaction_preserves_checkpoint_and_latest_result() {
    let contents = format!("preserved finding {}\n", "x".repeat(280)).repeat(200);
    let requests = child_scenario_with_contents(
        vec![
            large_read_response(0),
            large_read_response(1),
            chat_text_output_body("chat_summary_1", "first checkpoint finding"),
            large_read_response(2),
            chat_text_output_body("chat_summary_2", "second checkpoint finding"),
            chat_text_output_body("chat_final", "finished after two compactions"),
        ],
        &contents,
        &contents,
    )
    .await;

    assert_eq!(requests.len(), 7);
    for index in [2, 4] {
        let summary: serde_json::Value =
            serde_json::from_str(requests[index].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert!(summary.get("tools").is_none());
        assert_eq!(summary["model"], "glm-5.3-flash");
    }
    assert!(requests[4].contains("first checkpoint finding"));
    assert!(requests[5].contains("Untrusted lossy child checkpoint"));
    assert!(requests[5].contains("second checkpoint finding"));
    assert!(!requests[5].contains("first checkpoint finding"));
    assert!(requests[5].contains("preserved finding"));
    assert!(requests[5].contains("Inspect alpha.txt"));
    assert!(requests[6].contains("finished after two compactions"));
}

#[tokio::test(flavor = "current_thread")]
async fn checkpoint_append_failure_stops_child_without_replaying_completed_write() {
    child_compaction_stop_after_write(false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_child_compaction_stops_without_continuation_or_retry() {
    child_compaction_stop_after_write(true).await;
}

async fn child_compaction_stop_after_write(cancel: bool) {
    let root = TestRoot::new("child-checkpoint-failure");
    let selected = TestRoot::new("child-checkpoint-failure-selected");
    let written = "written before compaction\n";
    let contents = format!("preserved finding {}\n", "x".repeat(280)).repeat(200);
    fs::write(selected.path().join("alpha.txt"), &contents).unwrap();
    let store = SessionStore::open_for_test(root.path()).unwrap();
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0x81; 16]),
            0,
            b"test-key".to_vec(),
        )
        .await
        .unwrap();
    store
        .set_subagent_model_setting(
            PersistenceMutationRequestId::from_bytes([0x82; 16]),
            crate::persistence::SubagentModelSetting::Explicit {
                service: RunService::Go,
                model_id: "glm-5.3-flash".to_owned(),
            },
        )
        .await
        .unwrap();
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0x83; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .unwrap();
    let database_path = root.path().join("data/sessions.sqlite3");
    if !cancel {
        rusqlite::Connection::open(&database_path)
            .unwrap()
            .execute_batch(
                "ALTER TABLE child_journal ADD COLUMN reject_checkpoint INTEGER CHECK(kind != 7);",
            )
            .unwrap();
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (summary_tx, summary_rx) = oneshot::channel();
    let (stopped_tx, stopped_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let exchange = async {
            let (mut parent, _) = listener.accept().await.unwrap();
            read_http_request(&mut parent).await;
            let call = serde_json::json!({"id":"fc_task", "type":"function_call", "status":"completed", "call_id":"task_checkpoint_failure", "name":"task", "arguments": serde_json::json!({"context":"Read and report", "tasks":[{"name":"check", "task":"Inspect alpha.txt"}]}).to_string()});
            write_provider_output(&mut parent, "resp_task", &call.to_string()).await;

            let (mut child, _) = time::timeout(Duration::from_secs(10), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let first = String::from_utf8(read_http_request(&mut child).await).unwrap();
            assert!(!first.contains("Untrusted lossy child checkpoint"));
            let initial = tool_batch(vec![
                (
                    "write_before_compaction",
                    "write",
                    serde_json::json!({"path":"effect.txt", "content":written}),
                ),
                (
                    "read_0",
                    "read",
                    serde_json::json!({"path":"alpha.txt", "offset":1, "limit":200}),
                ),
            ]);
            write_provider_headers(&mut child, initial.len()).await;
            child.write_all(initial.as_bytes()).await.unwrap();
            child.shutdown().await.unwrap();

            let (mut second, _) = listener.accept().await.unwrap();
            let second_request = String::from_utf8(read_http_request(&mut second).await).unwrap();
            assert!(second_request.contains("write_before_compaction"));
            let response = large_read_response(1);
            write_provider_headers(&mut second, response.len()).await;
            second.write_all(response.as_bytes()).await.unwrap();
            second.shutdown().await.unwrap();

            let (mut summary, _) = listener.accept().await.unwrap();
            let request = String::from_utf8(read_http_request(&mut summary).await).unwrap();
            let body: serde_json::Value =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert!(body.get("tools").is_none());
            assert!(request.contains("write_before_compaction"));
            summary_tx.send(()).unwrap();
            if !cancel {
                let response =
                    chat_text_output_body("chat_summary", "summary retains the completed write");
                write_provider_headers(&mut summary, response.len()).await;
                summary.write_all(response.as_bytes()).await.unwrap();
                summary.shutdown().await.unwrap();
            }
            stopped_rx.await.unwrap();
            assert!(
                time::timeout(Duration::from_millis(500), listener.accept())
                    .await
                    .is_err(),
                "stopped child must not dispatch any continuation or retry"
            );
        };
        time::timeout(Duration::from_secs(30), exchange)
            .await
            .expect("bounded mock exchange");
    });
    let app = ServerApplication::from_session_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.id.as_bytes());
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) = app
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0x84; 16]),
            session_id,
            text: "Delegate a check".to_owned(),
            attachments: Vec::new(),
            service: ModelService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    time::timeout(Duration::from_secs(15), summary_rx)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        fs::read_to_string(selected.path().join("effect.txt")).unwrap(),
        written
    );
    if cancel {
        app.execute_for_local_owner(ApplicationRequest::CancelRun {
            mutation_request_id: MutationRequestId::from_bytes([0x85; 16]),
            session_id,
            run_id: run.id,
        })
        .await
        .unwrap();
    }
    if cancel {
        assert_eq!(
            wait_for_terminal(&app, session_id, run.id).await,
            RunState::Cancelled
        );
    } else {
        let mut shutdown = app.subscribe_shutdown_requests();
        time::timeout(
            Duration::from_secs(10),
            shutdown.wait_for(|requested| *requested),
        )
        .await
        .unwrap()
        .unwrap();
    }
    assert_eq!(
        fs::read_to_string(selected.path().join("effect.txt")).unwrap(),
        written
    );
    app.shutdown().await;
    drop(app);
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    let checkpoint_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM child_journal WHERE kind = 7",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let write_result_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM child_journal WHERE kind = 5",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(checkpoint_count, 0, "failed checkpoint must not commit");
    assert_eq!(
        write_result_count, 3,
        "one write and two reads completed exactly once"
    );
    let rows = child_journal_rows(&connection);
    assert_eq!(rows.iter().filter(|(kind, _)| *kind == 4).count(), 3);
    assert_eq!(rows.iter().filter(|(kind, _)| *kind == 2).count(), 3);
    if !cancel {
        assert_eq!(rows.last().unwrap().0, 3);
        assert!(
            String::from_utf8_lossy(&rows.last().unwrap().1)
                .contains("summary retains the completed write")
        );
        connection
            .execute_batch("ALTER TABLE child_journal DROP COLUMN reject_checkpoint;")
            .unwrap();
    }
    drop(connection);
    fs::write(
        selected.path().join("effect.txt"),
        "owner change after stop",
    )
    .unwrap();
    let reopened = SessionStore::open_for_test(root.path()).unwrap();
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    let recovered = child_journal_rows(&connection);
    assert_eq!(&recovered[..rows.len()], rows.as_slice());
    if cancel {
        assert_eq!(recovered, rows);
    } else {
        assert_eq!(recovered.len(), rows.len() + 1);
        assert_eq!(recovered.last().unwrap().0, 9);
    }
    assert_eq!(
        fs::read_to_string(selected.path().join("effect.txt")).unwrap(),
        "owner change after stop"
    );
    assert_eq!(
        fs::read_to_string(selected.path().join("alpha.txt")).unwrap(),
        contents
    );
    drop(reopened);
    stopped_tx.send(()).unwrap();
    time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

fn child_journal_rows(connection: &rusqlite::Connection) -> Vec<(i64, Vec<u8>)> {
    connection
        .prepare("SELECT kind, payload FROM child_journal ORDER BY ordinal")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn read_response(index: usize, invalid: bool) -> String {
    let response = chat_tool_output_body(&format!("chat_{index}"))
        .replace("provider_child_read", &format!("read_{index}"));
    if invalid {
        response.replace("\\\"offset\\\":1", "\\\"offset\\\":0")
    } else {
        response
    }
}

fn large_read_response(index: usize) -> String {
    read_response(index, false).replace("\\\"limit\\\":10", "\\\"limit\\\":200")
}

#[tokio::test(flavor = "current_thread")]
async fn child_corrects_rejected_read_without_replaying_prior_calls() {
    let requests = child_scenario(vec![
        read_response(0, false),
        read_response(1, true),
        chat_text_output_body("chat_final", "partial finding"),
    ])
    .await;
    assert!(requests[2].contains("read offset or limit"));
    assert!(requests[2].contains("preserved finding"));
    assert!(!requests[2].contains("read_1"));
    assert_eq!(requests[2].matches("tool_call_id\":\"read_0").count(), 1);
    assert!(requests[3].contains("partial finding"));
}

#[tokio::test(flavor = "current_thread")]
async fn child_stops_after_second_invalid_response() {
    let requests = child_scenario(vec![read_response(0, true), read_response(1, true)]).await;
    assert!(requests[2].contains("read offset or limit"));
    assert!(requests[2].contains("no tools from this response executed"));
    assert!(!requests[1].contains("preserved finding"));
}

#[tokio::test(flavor = "current_thread")]
async fn child_keeps_tools_and_succeeds_beyond_former_turn_limit() {
    let mut responses: Vec<_> = (0..10).map(|i| read_response(i, false)).collect();
    responses.push(chat_text_output_body("chat_final", "retained findings"));
    let requests = child_scenario(responses).await;
    let last_child = &requests[requests.len() - 2];
    let body: serde_json::Value =
        serde_json::from_str(last_child.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert!(!body["tools"].as_array().unwrap().is_empty());
    assert!(!last_child.contains("REPORT ONLY"));
    assert!(requests.last().unwrap().contains("retained findings"));
    assert!(requests.last().unwrap().contains("succeeded"));
}
