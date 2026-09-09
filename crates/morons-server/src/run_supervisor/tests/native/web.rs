use super::*;

#[tokio::test(flavor = "current_thread")]
async fn astra_parent_glm_flash_child_uses_isolated_openai_web_and_separate_receipts() {
    child_web_flow(false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn uncertain_child_web_stops_the_outer_task_and_parent_without_replay() {
    child_web_flow(true).await;
}

async fn child_web_flow(uncertain: bool) {
    let root = TestRoot::new("astra-glm-web");
    let selected = TestRoot::new("astra-glm-web-selected");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0xc0; 16]),
            0,
            b"synthetic-glm-web-key".to_vec(),
        )
        .await
        .unwrap();
    for generation in 0..2 {
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
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        time::timeout(Duration::from_secs(30),async move {
        for step in 0..5 {
            let (mut stream,_)=listener.accept().await.unwrap();
            let request=String::from_utf8(read_http_request(&mut stream).await).unwrap();
            let (headers,body)=request.split_once("\r\n\r\n").unwrap();let body:serde_json::Value=serde_json::from_str(body).unwrap();
            match step {
                0|4=>{
                    assert!(request.starts_with("POST /backend-api/codex/responses"));assert_eq!(body["model"],"gpt-6-astra");
                    if step==4 { assert!(request.contains("Fixture child report"));assert!(request.contains("web_searches"));assert!(request.contains("glm-5.3-flash")); }
                    let output=if step==0 { serde_json::json!({"id":"fc_task_web","type":"function_call","status":"completed","call_id":"call_task_web","name":"task","arguments":serde_json::json!({"context":"PRIVATE_TASK_CONTEXT","tasks":[{"name":null,"task":"Search public web and report"}]}).to_string()}) } else { serde_json::json!({"id":"msg_parent_web","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Parent report","annotations":[]}]}) };
                    write_native_model(&mut stream,&format!("resp_parent_{step}"),&output.to_string(),"gpt-6-astra").await;
                }
                1|3=>{
                    assert!(request.starts_with("POST /zen/go/v1/chat/completions"));assert_eq!(body["model"],"glm-5.3-flash");assert!(!headers.contains("chatgpt-account-id"));
                    if step==3 { assert!(request.contains("Fixture answer."));assert!(request.contains("https://example.com/source")); }
                    let delta=if step==1 { serde_json::json!({"role":"assistant","tool_calls":[{"index":0,"id":"call_child_web","type":"function","function":{"name":"web_search","arguments":"{\"query\":\"public query\"}"}}]}) } else {serde_json::json!({"role":"assistant","content":"Fixture child report"})};
                    let chunk=serde_json::json!({"id":format!("chat_{step}"),"object":"chat.completion.chunk","created":1,"model":"glm-5.3-flash","choices":[{"index":0,"delta":delta,"finish_reason":if step==1 {"tool_calls"}else{"stop"}}],"usage":null});
                    let trailer=serde_json::json!({"id":format!("chat_{step}"),"object":"chat.completion.chunk","created":1,"model":"glm-5.3-flash","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}});
                    let body=format!("data: {chunk}\n\ndata: {trailer}\n\ndata: [DONE]\n\n");
                    write_provider_headers(&mut stream,body.len()).await;stream.write_all(body.as_bytes()).await.unwrap();stream.shutdown().await.unwrap();
                }
                2=>{
                    assert!(request.starts_with("POST /backend-api/codex/responses"));assert_eq!(body["model"],"gpt-5.5");assert_eq!(body["tools"].as_array().unwrap().len(),1);assert_eq!(body["tools"][0]["type"],"web_search");assert_eq!(request_header(&request,"originator"),"morons");
                    assert!(!request.contains("PRIVATE_PARENT"));assert!(!request.contains("PRIVATE_TASK_CONTEXT"));assert!(!request.contains("synthetic-glm-web-key"));
                    let body=if uncertain { b"data: [DONE]\n\n".to_vec() } else { crate::provider::openai_web::response_sources_fixture() };write_provider_headers(&mut stream,body.len()).await;stream.write_all(&body).await.unwrap();stream.shutdown().await.unwrap();
                    if uncertain { assert!(time::timeout(Duration::from_millis(200),listener.accept()).await.is_err()); break; }
                }
                _=>unreachable!(),
            }
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
            service: ModelService::OpenAiChatGpt,
            model_id: "gpt-6-astra".into(),
        })
        .await
        .unwrap();
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        response
    else {
        panic!("accepted")
    };
    assert_eq!(
        wait_for_terminal(&app, session_id, run.id).await,
        if uncertain {
            RunState::Uncertain
        } else {
            RunState::Succeeded
        }
    );
    server.await.unwrap();
    app.shutdown().await;
    drop(app);
    let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    let generations:(i64,i64)=db.query_row("SELECT task.credential_generation,web.credential_generation FROM task_model_bindings AS task JOIN web_model_bindings AS web USING(call_id)",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(generations, (1, 2));
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
        assert_eq!(
            value,
            serde_json::json!({"status":"error","error":{"web_search_uncertain":{"stage":"termination","category":"malformed_response"}}})
        );
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
    assert_eq!(results[0].status, crate::tools::SubagentStatus::Succeeded);
    assert_eq!(results[0].usage.total_tokens, 24);
    assert_eq!(results[0].web_searches.len(), 1);
    assert_eq!(results[0].web_searches[0].usage.total_tokens, 16);
    assert!(SessionStore::open_for_test(root.path()).is_ok());
}
