use super::*;

#[tokio::test(flavor = "current_thread")]
async fn astra_parent_glm_flash_child_uses_isolated_openai_web_and_separate_receipts() {
    child_web_flow(false, false, false, Limit::None).await;
}

#[tokio::test(flavor = "current_thread")]
async fn uncertain_child_web_stops_the_outer_task_and_parent_without_replay() {
    child_web_flow(false, true, false, Limit::None).await;
}

#[tokio::test(flavor = "current_thread")]
async fn exa_child_web_commits_separate_accounting_and_resumes_parent() {
    child_web_flow(true, false, false, Limit::None).await;
}

#[tokio::test(flavor = "current_thread")]
async fn uncertain_exa_child_web_stops_parent_without_replay() {
    child_web_flow(true, true, false, Limit::None).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_exa_child_web_preserves_uncertainty_without_parent_continuation() {
    child_web_flow(true, true, true, Limit::None).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_native_child_web_preserves_uncertainty_without_parent_continuation() {
    child_web_flow(false, true, true, Limit::None).await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_child_report_limit_preserves_completed_search_accounting() {
    child_web_flow(false, false, false, Limit::Report).await;
}

#[tokio::test(flavor = "current_thread")]
async fn exa_child_report_limit_preserves_completed_search_accounting() {
    child_web_flow(true, false, false, Limit::Report).await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_child_exceeds_former_tool_quota_and_preserves_accounting() {
    child_web_flow(false, false, false, Limit::ToolCalls).await;
}

#[tokio::test(flavor = "current_thread")]
async fn exa_child_exceeds_former_tool_quota_and_preserves_accounting() {
    child_web_flow(true, false, false, Limit::ToolCalls).await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_child_exceeds_former_turn_quota_with_search_accounting() {
    child_web_flow(false, false, false, Limit::ProviderTurns).await;
}

#[tokio::test(flavor = "current_thread")]
async fn exa_child_exceeds_former_turn_quota_with_search_accounting() {
    child_web_flow(true, false, false, Limit::ProviderTurns).await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_child_continued_tools_allow_search_without_losing_accounting() {
    child_web_flow(false, false, false, Limit::ContinuedTools).await;
}

#[tokio::test(flavor = "current_thread")]
async fn exa_child_continued_tools_allow_search_without_losing_accounting() {
    child_web_flow(true, false, false, Limit::ContinuedTools).await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_child_completion_write_failure_stops_supervisor_without_replay() {
    child_web_flow(false, false, false, Limit::CompletionWrite).await;
}

#[tokio::test(flavor = "current_thread")]
async fn exa_child_completion_write_failure_stops_supervisor_without_replay() {
    child_web_flow(true, false, false, Limit::CompletionWrite).await;
}

#[tokio::test]
async fn native_child_cancellation_after_search_dispatch_preserves_uncertainty() {
    child_web_flow(false, true, false, Limit::Cancellation).await;
}

#[tokio::test]
async fn exa_child_cancellation_after_search_dispatch_preserves_uncertainty() {
    child_web_flow(true, true, false, Limit::Cancellation).await;
}

#[tokio::test]
async fn native_web_cancellation_drains_pending_sibling_without_parent_continuation() {
    child_web_flow(false, true, false, Limit::SiblingCancellation).await;
}

#[tokio::test]
async fn exa_web_cancellation_drains_pending_sibling_without_parent_continuation() {
    child_web_flow(true, true, false, Limit::SiblingCancellation).await;
}

#[tokio::test]
async fn native_web_uncertainty_drains_pending_sibling_without_parent_continuation() {
    child_web_flow(false, true, false, Limit::SiblingUncertain).await;
}

#[tokio::test]
async fn exa_web_uncertainty_drains_pending_sibling_without_parent_continuation() {
    child_web_flow(true, true, false, Limit::SiblingUncertain).await;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Limit {
    None,
    Report,
    ToolCalls,
    ProviderTurns,
    ContinuedTools,
    CompletionWrite,
    Cancellation,
    SiblingCancellation,
    SiblingUncertain,
}

async fn child_web_flow(exa: bool, uncertain: bool, cancel: bool, limit: Limit) {
    let cancellation = matches!(limit, Limit::Cancellation | Limit::SiblingCancellation);
    let sibling = matches!(limit, Limit::SiblingCancellation | Limit::SiblingUncertain);
    let completion_failure = limit == Limit::CompletionWrite;
    let report_limit = limit == Limit::Report;
    let tool_limit = limit == Limit::ToolCalls;
    let continued_tools = limit == Limit::ContinuedTools;
    let turn_limit = matches!(limit, Limit::ProviderTurns | Limit::ContinuedTools);
    let root = TestRoot::new("astra-glm-web");
    let selected = TestRoot::new("astra-glm-web-selected");
    if tool_limit || turn_limit {
        std::fs::write(selected.path().join("budget.txt"), "budget fixture").unwrap();
    }
    let store = SessionStore::open_for_test(root.path()).unwrap();
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0xc0; 16]),
            0,
            b"synthetic-glm-web-key".to_vec(),
        )
        .await
        .unwrap();
    for generation in 0..if exa { 0 } else { 2 } {
        store
            .set_openai_credential(
                PersistenceMutationRequestId::from_bytes(
                    [0xc1 + u8::try_from(generation).unwrap(); 16],
                ),
                generation,
                synthetic_tokens(),
            )
            .await
            .unwrap();
    }
    store
        .set_subagent_model_setting(
            PersistenceMutationRequestId::from_bytes([0xc3; 16]),
            crate::persistence::SubagentModelSetting::Explicit {
                service: crate::persistence::RunService::Go,
                model_id: "glm-5.3-flash".into(),
            },
        )
        .await
        .unwrap();
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0xc4; 16]),
            None,
            selected.path().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    if completion_failure {
        let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        db.execute_batch("CREATE TRIGGER reject_web_success BEFORE INSERT ON web_search_successes BEGIN SELECT RAISE(ABORT, 'test completion failure'); END;").unwrap();
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (request_seen_tx, request_seen_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut request_seen_tx = Some(request_seen_tx);
        let mut pending_sibling = None;
        let mut release_rx = Some(release_rx);
        time::timeout(Duration::from_secs(30),async move {
        let final_step = if turn_limit { 12 + 2 } else if tool_limit { 8 } else { 4 };
        for step in 0..=final_step {
            let (mut stream, request) = loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = String::from_utf8(read_http_request(&mut stream).await).unwrap();
                if step == 2 && sibling && request.starts_with("POST /zen/go/v1/chat/completions") {
                    assert!(pending_sibling.is_none());
                    assert!(request.contains("glm-5.3-flash"));
                    write_provider_headers(&mut stream, 4096).await;
                    pending_sibling = Some(stream);
                    continue;
                }
                break (stream, request);
            };
            let (headers,body)=request.split_once("\r\n\r\n").unwrap();let body:serde_json::Value=serde_json::from_str(body).unwrap();
            match step {
                step if step == 0 || step == final_step =>{
                    let parent_model = if exa { "muse-spark-1.2" } else { "gpt-6-astra" };
                    assert!(request.starts_with(if exa { "POST /zen/v1/responses" } else { "POST /backend-api/codex/responses" }));assert_eq!(body["model"],parent_model);
                    if step == final_step {
                        assert!(request.contains(if report_limit { "subagent final report exceeded the output limit" } else { "Fixture child report" }));
                        if report_limit {
                            assert!(request.contains("resource_limit"));
                        }
                        if tool_limit || (turn_limit && !continued_tools) {
                            assert!(!request.contains("partial report:"));
                        }
                        assert!(request.contains(if exa { "exa_searches" } else { "web_searches" }));
                        assert!(request.contains("glm-5.3-flash"));
                    }
                    let output=if step==0 { serde_json::json!({"id":"fc_task_web","type":"function_call","status":"completed","call_id":"call_task_web","name":"task","arguments":serde_json::json!({"context":"PRIVATE_TASK_CONTEXT","tasks": if sibling { vec![serde_json::json!({"name":null,"task":"Search public web and report"}); 2] } else { vec![serde_json::json!({"name":null,"task":"Search public web and report"})] }}).to_string()}) } else { serde_json::json!({"id":"msg_parent_web","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Parent report","annotations":[]}]}) };
                    write_native_model(&mut stream,&format!("resp_parent_{step}"),&output.to_string(),parent_model).await;
                }
                step if step == 1 || (3..final_step).contains(&step) =>{
                    assert!(request.starts_with("POST /zen/go/v1/chat/completions"));assert_eq!(body["model"],"glm-5.3-flash");assert!(!headers.contains("chatgpt-account-id"));
                    assert_eq!(body["max_tokens"], 8192);
                    if step==3 { assert!(request.contains("Fixture answer."));assert!(request.contains("https://example.com/source")); }
                    if (tool_limit || turn_limit) && step == final_step - 1 {
                        assert!(!request.contains("REPORT ONLY"));
                        if tool_limit {
                            assert!(!request.contains("None of its calls executed"));
                            assert!(request.contains("read_6_"));
                        }
                        assert!(!body["tools"].as_array().unwrap().is_empty());
                    }
                    let delta=if step==1 { serde_json::json!({"role":"assistant","tool_calls":[{"index":0,"id":"call_child_web","type":"function","function":{"name":"web_search","arguments":"{\"query\":\"public query\"}"}}]}) } else if tool_limit && step <= 6 {
                        let calls: Vec<_> = (0..8).map(|index| serde_json::json!({"index":index,"id":format!("read_{step}_{index}"),"type":"function","function":{"name":"read","arguments":serde_json::json!({"path":"budget.txt","offset":1,"limit":1}).to_string()}})).collect();
                        serde_json::json!({"role":"assistant","tool_calls":calls})
                    } else if turn_limit && step < final_step - 1 {
                        serde_json::json!({"role":"assistant","tool_calls":[{"index":0,"id":format!("read_{step}"),"type":"function","function":{"name":"read","arguments":serde_json::json!({"path":"budget.txt","offset":1,"limit":1}).to_string()}}]})
                    } else {serde_json::json!({"role":"assistant","content": if report_limit { "x".repeat(crate::tools::MAX_SUBAGENT_OUTPUT_BYTES + 1) } else { "Fixture child report".into() }})};
                    let chunk=serde_json::json!({"id":format!("chat_{step}"),"object":"chat.completion.chunk","created":1,"model":"glm-5.3-flash","choices":[{"index":0,"delta":delta,"finish_reason":if delta.get("tool_calls").is_some() {"tool_calls"}else{"stop"}}],"usage":null});
                    let trailer=serde_json::json!({"id":format!("chat_{step}"),"object":"chat.completion.chunk","created":1,"model":"glm-5.3-flash","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}});
                    let body=format!("data: {chunk}\n\ndata: {trailer}\n\ndata: [DONE]\n\n");
                    write_provider_headers(&mut stream,body.len()).await;stream.write_all(body.as_bytes()).await.unwrap();stream.shutdown().await.unwrap();
                }
                2=>{
                    if sibling && pending_sibling.is_none() {
                        let (mut sibling_stream, _) = listener.accept().await.unwrap();
                        let sibling_request = String::from_utf8(read_http_request(&mut sibling_stream).await).unwrap();
                        assert!(sibling_request.starts_with("POST /zen/go/v1/chat/completions"));
                        write_provider_headers(&mut sibling_stream, 4096).await;
                        pending_sibling = Some(sibling_stream);
                    }
                    if exa {
                        assert!(request.starts_with("POST /mcp?tools=web_search_exa"));
                        assert_eq!(body["params"]["arguments"]["query"], "public query");
                        assert!(!headers.to_ascii_lowercase().contains("authorization:"));
                        assert!(!headers.contains("chatgpt-account-id"));
                    } else {
                    assert!(request.starts_with("POST /backend-api/codex/responses"));assert_eq!(body["model"],"gpt-5.5");assert_eq!(body["tools"].as_array().unwrap().len(),1);assert_eq!(body["tools"][0]["type"],"web_search");assert_eq!(request_header(&request,"originator"),"morons");
                    }
                    assert!(!request.contains("PRIVATE_PARENT"));assert!(!request.contains("PRIVATE_TASK_CONTEXT"));assert!(!request.contains("synthetic-glm-web-key"));
                    if cancel || cancellation {
                        request_seen_tx.take().unwrap().send(()).unwrap();
                        release_rx.take().unwrap().await.unwrap();
                        assert!(time::timeout(Duration::from_millis(200),listener.accept()).await.is_err());
                        break;
                    }
                    let body=if uncertain { b"data: [DONE]\n\n".to_vec() } else if exa {
                        serde_json::to_vec(&serde_json::json!({"jsonrpc":"2.0","id":1,"result":{"structuredContent":{"results":[{"title":"Fixture source","url":"https://example.com/source","text":"Fixture answer."}]}}})).unwrap()
                    } else { crate::provider::openai_web::response_sources_fixture() };
                    if exa {
                        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
                    } else { write_provider_headers(&mut stream,body.len()).await; }
                    stream.write_all(&body).await.unwrap();stream.shutdown().await.unwrap();
                    if uncertain || completion_failure { assert!(time::timeout(Duration::from_millis(200),listener.accept()).await.is_err()); break; }
                }
                _=>unreachable!(),
            }
        }
        if let Some(mut sibling_stream) = pending_sibling {
            let mut byte = [0; 1];
            assert_eq!(time::timeout(Duration::from_secs(5), sibling_stream.read(&mut byte)).await.unwrap().unwrap(), 0);
        }
        if tool_limit || turn_limit {
            assert!(time::timeout(Duration::from_millis(200), listener.accept()).await.is_err());
        }
    }).await.unwrap();
    });
    let app = ServerApplication::from_native_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.id.as_bytes());
    let response = app
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0xc5; 16]),
            session_id,
            text: "PRIVATE_PARENT delegate research".into(),
            attachments: Vec::new(),
            service: if exa {
                ModelService::Zen
            } else {
                ModelService::OpenAiChatGpt
            },
            model_id: if exa { "muse-spark-1.2" } else { "gpt-6-astra" }.into(),
        })
        .await
        .unwrap();
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        response
    else {
        panic!("accepted")
    };
    if cancel || cancellation {
        time::timeout(Duration::from_secs(10), request_seen_rx)
            .await
            .unwrap()
            .unwrap();
        app.execute_for_local_owner(ApplicationRequest::CancelRun {
            mutation_request_id: MutationRequestId::from_bytes([0xc6; 16]),
            session_id,
            run_id: run.id,
        })
        .await
        .unwrap();
    }
    if completion_failure {
        let mut shutdown = app.subscribe_shutdown_requests();
        time::timeout(
            Duration::from_secs(10),
            shutdown.wait_for(|requested| *requested),
        )
        .await
        .unwrap()
        .unwrap();
    } else {
        assert_eq!(
            wait_for_terminal(&app, session_id, run.id).await,
            if uncertain {
                RunState::Uncertain
            } else {
                RunState::Succeeded
            }
        );
    }
    if cancel || cancellation {
        release_tx.send(()).unwrap();
    }
    server.await.unwrap();
    app.shutdown().await;
    drop(app);
    let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    let generations:(i64,i64)=db.query_row("SELECT task.credential_generation,web.credential_generation FROM task_model_bindings AS task JOIN web_model_bindings AS web USING(call_id)",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(generations, (1, if exa { 0 } else { 2 }));
    assert_eq!(
        db.query_row("SELECT admission_count FROM web_model_bindings", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM web_search_attempts", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    if completion_failure {
        let counts: (i64, i64, i64) = db.query_row(
            "SELECT success_count,(SELECT COUNT(*) FROM web_search_successes),(SELECT COUNT(*) FROM tool_operation_facts WHERE fact_kind=3) FROM web_model_bindings",
            [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap();
        assert_eq!(counts, (0, 0, 0));
        db.execute_batch("DROP TRIGGER reject_web_success;")
            .unwrap();
        drop(db);
        let reopened = SessionStore::open_for_test(root.path()).unwrap();
        assert_eq!(
            reopened
                .get_run(
                    session.id,
                    crate::persistence::RunId::from_bytes(*run.id.as_bytes())
                )
                .await
                .unwrap()
                .unwrap()
                .state,
            crate::persistence::RunState::Uncertain
        );
        drop(reopened);
        drop(SessionStore::open_for_test(root.path()).unwrap());
        return;
    }
    if uncertain {
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM tool_operation_facts WHERE fact_kind=6",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM provider_operation_facts WHERE fact_kind=2",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        let payload: Vec<u8> = db
            .query_row(
                "SELECT result_payload FROM tool_operation_facts WHERE fact_kind=6",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        if exa {
            assert_eq!(
                value,
                serde_json::json!({"status":"error","error":"exa_search_uncertain"})
            );
        } else if !cancel && !cancellation {
            assert_eq!(
                value,
                serde_json::json!({"status":"error","error":{"web_search_uncertain":{"stage":"termination","category":"malformed_response"}}})
            );
        } else {
            assert!(value["error"].get("web_search_uncertain").is_some());
        }
        assert!(SessionStore::open_for_test(root.path()).is_ok());
        return;
    }
    let payload: Vec<u8> = db
        .query_row(
            "SELECT result_payload FROM tool_operation_facts WHERE fact_kind=3",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let result: crate::tools::ToolResult = serde_json::from_slice(&payload).unwrap();
    let crate::tools::ToolResult::Ok {
        output: crate::tools::ToolOutput::Task { results },
    } = result
    else {
        panic!("task result")
    };
    assert_eq!(
        results[0].status,
        if report_limit {
            crate::tools::SubagentStatus::ResourceLimit
        } else {
            crate::tools::SubagentStatus::Succeeded
        }
    );
    let expected_turns = if turn_limit {
        12
    } else if tool_limit {
        6
    } else {
        2
    };
    assert_eq!(results[0].provider_turns, expected_turns);
    assert_eq!(results[0].usage.total_tokens, expected_turns * 12);
    assert_eq!(
        results[0].tool_calls,
        if turn_limit {
            expected_turns - 1
        } else if tool_limit {
            33
        } else {
            1
        }
    );
    assert_eq!(results[0].tool_mutations, 0);
    assert_eq!(results[0].exa_searches, u64::from(exa));
    assert_eq!(results[0].web_searches.len(), usize::from(!exa));
    if !exa {
        assert_eq!(results[0].web_searches[0].usage.total_tokens, 16);
    }
    assert!(SessionStore::open_for_test(root.path()).is_ok());
}
