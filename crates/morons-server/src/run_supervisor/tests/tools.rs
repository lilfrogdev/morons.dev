use super::*;

#[tokio::test(flavor = "current_thread")]
async fn missing_write_parent_returns_known_feedback_without_replaying_a_mutation() {
    let (root, selected, store, session) = super::hardening::fixture("write-feedback").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let provider = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        read_http_request(&mut first).await;
        let call = serde_json::json!({"type":"function_call", "id":"fc_write", "call_id":"call_write", "name":"write", "arguments":r#"{"path":"missing/file.txt","content":"new"}"#, "status":"completed"});
        write_provider_output(&mut first, "resp_write", &call.to_string()).await;
        let (mut second, _) = listener.accept().await.unwrap();
        let request = String::from_utf8(read_http_request(&mut second).await).unwrap();
        let body: serde_json::Value =
            serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        let feedback = body["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == "function_call_output")
            .unwrap();
        let feedback: serde_json::Value =
            serde_json::from_str(feedback["output"].as_str().unwrap()).unwrap();
        assert_eq!(
            feedback,
            serde_json::json!({"status":"error", "error":"not_found"})
        );
        write_provider_output(&mut second, "resp_report", r#"{"id":"msg_report","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"The parent directory is missing; no write was performed.","annotations":[]}]}"#).await;
    });
    let application = ServerApplication::from_session_store_for_test(store, &origin);
    let session_id = SessionId::from_bytes(*session.as_bytes());
    let outcome = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([1; 16]),
            session_id,
            text: "Try writing missing/file.txt once and report the result.".to_owned(),
            attachments: Vec::new(),
            service: ModelService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .unwrap();
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        outcome
    else {
        panic!("expected run");
    };
    assert_eq!(
        wait_for_terminal(&application, session_id, run.id).await,
        RunState::Succeeded
    );
    provider.await.unwrap();
    assert!(!selected.path().join("missing").exists());
    application.shutdown().await;
    drop(application);
    SessionStore::open_for_test(root.path()).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn direct_tool_loop_reads_edits_runs_bash_and_commits_durable_results() {
    let root = TestRoot::new("direct-tool-loop");
    let selected = TestRoot::new("direct-tool-directory");
    fs::write(selected.path().join("note.txt"), "before\n")
        .expect("selected file should be written");
    let store = SessionStore::open_for_test(root.path()).expect("session store should open");
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0x81; 16]),
            0,
            b"not-a-real-tool-loop-key".to_vec(),
        )
        .await
        .expect("credential should be configured");
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0x82; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let (base, requests, provider_task) = spawn_direct_tool_loop_provider().await;
    let application = ServerApplication::from_session_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.id.as_bytes());
    let accepted = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0x83; 16]),
            session_id,
            text: "inspect and update note.txt".to_owned(),
            attachments: Vec::new(),
            service: ModelService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .expect("tool run should be accepted");
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        accepted
    else {
        panic!("input should return a run");
    };
    assert_eq!(run.tool_catalog_version, crate::tools::TOOL_CATALOG_VERSION);
    assert_eq!(
        wait_for_terminal(&application, session_id, run.id).await,
        RunState::Succeeded
    );
    provider_task.await.expect("tool provider should finish");
    let requests = requests.await.expect("tool requests should be captured");
    assert_eq!(requests.len(), 4);
    assert!(requests[0].contains("\"name\":\"read\""));
    assert!(requests[1].contains("function_call_output"));
    assert!(requests[2].contains("\"name\":\"edit\""));
    assert!(requests[2].contains("edited"));
    assert!(requests[3].contains("\"name\":\"bash\""));
    assert!(requests[3].contains("shell stdout"));
    assert!(!requests.iter().any(|request| request.contains("read_file")));
    assert_eq!(
        fs::read_to_string(selected.path().join("note.txt"))
            .expect("selected file should remain readable"),
        "after\n"
    );
    assert_eq!(
        fs::read_to_string(selected.path().join("shell.txt"))
            .expect("bash output file should remain readable"),
        "shell"
    );

    let mut cursor = None;
    let mut entries = Vec::new();
    loop {
        let outcome = application
            .execute_for_local_owner(ApplicationRequest::ListSessionTranscript {
                session_id,
                cursor,
                direction: morons_protocol::TranscriptPageDirection::Newer,
                limit: 1,
            })
            .await
            .expect("tool transcript should page");
        let ApplicationOutcome::Response(ApplicationResponse::SessionTranscriptListed {
            entries: page,
            newer_cursor,
            ..
        }) = outcome
        else {
            panic!("transcript should return a page");
        };
        entries.extend(page);
        let Some(next) = newer_cursor else { break };
        cursor = Some(next);
    }
    assert_eq!(entries.len(), 8);
    assert!(matches!(
        entries[1],
        morons_protocol::TranscriptEntry::ToolCall {
            tool: morons_protocol::ToolKind::Read,
            ..
        }
    ));
    assert!(matches!(
        entries[3],
        morons_protocol::TranscriptEntry::ToolCall {
            tool: morons_protocol::ToolKind::Edit,
            ..
        }
    ));
    assert!(matches!(
        entries[5],
        morons_protocol::TranscriptEntry::ToolCall {
            tool: morons_protocol::ToolKind::Bash,
            ..
        }
    ));
    for index in [2, 4, 6] {
        assert!(matches!(
            entries[index],
            morons_protocol::TranscriptEntry::ToolResult {
                status: morons_protocol::ToolResultStatus::Succeeded,
                ..
            }
        ));
    }
    application.shutdown().await;
    drop(application);
    SessionStore::open_for_test(root.path()).expect("durable tool history should reopen");
}

#[tokio::test(flavor = "current_thread")]
async fn read_image_tool_stores_bytes_outside_sqlite_and_returns_multimodal_content() {
    let root = TestRoot::new("read-image-tool");
    let selected = TestRoot::new("read-image-directory");
    let image =
        morons_image::normalize_rgba(3, 2, vec![0x66; 24]).expect("fixture image should normalize");
    fs::write(selected.path().join("picture.png"), &image.bytes)
        .expect("fixture image should be written");
    let store = SessionStore::open_for_test(root.path()).expect("session store should open");
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0xd1; 16]),
            0,
            b"not-a-real-read-image-key".to_vec(),
        )
        .await
        .expect("credential should be configured");
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0xd2; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let (base, requests, provider_task) = spawn_read_image_tool_provider().await;
    let application = ServerApplication::from_session_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.id.as_bytes());
    let accepted = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0xd3; 16]),
            session_id,
            text: "inspect picture.png".to_owned(),
            attachments: Vec::new(),
            service: ModelService::Zen,
            model_id: "gpt-5.4".to_owned(),
        })
        .await
        .expect("image tool run should be accepted");
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        accepted
    else {
        panic!("input should return a run");
    };
    assert_eq!(
        wait_for_terminal(&application, session_id, run.id).await,
        RunState::Succeeded
    );
    provider_task.await.expect("provider fixture should finish");
    let requests = requests.await.expect("requests should be captured");
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("\"name\":\"read\""));
    assert!(requests[1].contains("function_call_output"));
    assert!(requests[1].contains("data:image/png;base64,"));
    assert!(requests[1].contains("[picture.png]"));
    application.shutdown().await;
    drop(application);
    let database =
        fs::read(root.path().join("data/sessions.sqlite3")).expect("database should be readable");
    assert!(!contains_bytes(&database, &image.bytes));
    assert_eq!(
        fs::read_dir(root.path().join("attachments"))
            .expect("attachment directory should be readable")
            .count(),
        1
    );
    SessionStore::open_for_test(root.path()).expect("read image result should reopen");
}

#[tokio::test(flavor = "current_thread")]
async fn web_search_tool_uses_reviewed_adapter_and_commits_cited_results() {
    let root = TestRoot::new("web-search-tool-loop");
    let selected = TestRoot::new("web-search-directory");
    fs::write(selected.path().join("keep.txt"), "keep").unwrap();
    let store = SessionStore::open_for_test(root.path()).expect("session store should open");
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0x91; 16]),
            0,
            b"not-a-real-web-tool-key".to_vec(),
        )
        .await
        .expect("credential should be configured");
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0x92; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    store
        .set_openai_credential(
            PersistenceMutationRequestId::from_bytes([0x94; 16]),
            0,
            super::native::synthetic_tokens(),
        )
        .await
        .unwrap();
    let (provider_base, provider_requests, provider_task) =
        spawn_web_search_tool_loop_provider().await;
    let (search_origin, search_request, search_task) = spawn_search_adapter().await;
    let application = ServerApplication::from_session_store_with_search_for_test(
        store,
        &provider_base,
        search_origin,
    );
    let session_id = SessionId::from_bytes(*session.id.as_bytes());
    let accepted = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0x93; 16]),
            session_id,
            text: "find the current Rust site".to_owned(),
            attachments: Vec::new(),
            service: ModelService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .expect("web search run should be accepted");
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        accepted
    else {
        panic!("input should return a run");
    };
    assert_eq!(
        wait_for_terminal(&application, session_id, run.id).await,
        RunState::Succeeded
    );
    search_task.await.expect("search fixture should finish");
    provider_task.await.expect("provider fixture should finish");
    let search_request = search_request
        .await
        .expect("search request should be captured");
    assert!(search_request.starts_with("POST /search HTTP/1.1"));
    assert!(search_request.contains("\"type\":\"web_search\""));
    assert!(search_request.contains("\"model\":\"gpt-5.5\""));
    assert!(!search_request.contains("find the current Rust site"));
    assert!(!search_request.contains("x-subscription-token"));
    let provider_requests = provider_requests
        .await
        .expect("provider requests should be captured");
    assert_eq!(provider_requests.len(), 2);
    assert!(provider_requests[0].contains("\"name\":\"web_search\""));
    assert!(provider_requests[1].contains("https://example.com/source"));
    assert!(provider_requests[1].contains("Fixture answer."));
    assert!(provider_requests[1].contains("\\\"receipt\\\""));
    assert!(
        !provider_requests
            .iter()
            .any(|request| request.contains("not-a-real-search-key"))
    );

    let mut cursor = None;
    let mut entries = Vec::new();
    loop {
        let outcome = application
            .execute_for_local_owner(ApplicationRequest::ListSessionTranscript {
                session_id,
                cursor,
                direction: morons_protocol::TranscriptPageDirection::Newer,
                limit: 1,
            })
            .await
            .expect("transcript should load");
        let ApplicationOutcome::Response(ApplicationResponse::SessionTranscriptListed {
            entries: page,
            newer_cursor,
            ..
        }) = outcome
        else {
            panic!("transcript should return a page");
        };
        entries.extend(page);
        let Some(next) = newer_cursor else { break };
        cursor = Some(next);
    }
    assert!(matches!(
        entries[1],
        morons_protocol::TranscriptEntry::ToolCall {
            tool: morons_protocol::ToolKind::WebSearch,
            ..
        }
    ));
    assert!(matches!(
        entries[2],
        morons_protocol::TranscriptEntry::ToolResult {
            status: morons_protocol::ToolResultStatus::Succeeded,
            ..
        }
    ));
    if let morons_protocol::TranscriptEntry::ToolResult { summary, .. } = &entries[2] {
        assert!(summary.contains("https://example.com/source"));
        assert!(summary.contains("Fixture answer."));
        assert!(summary.contains("separate from coding usage"));
    }
    application.shutdown().await;
    drop(application);
    let store = SessionStore::open_for_test(root.path()).expect("web search history should reopen");
    store
        .set_session_archived(
            PersistenceMutationRequestId::from_bytes([0x95; 16]),
            session.id,
            true,
        )
        .await
        .unwrap();
    store
        .delete_session(
            PersistenceMutationRequestId::from_bytes([0x96; 16]),
            session.id,
        )
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(selected.path().join("keep.txt")).unwrap(),
        "keep"
    );
    let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM web_model_bindings", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let database = fs::read(root.path().join("data").join("sessions.sqlite3"))
        .expect("database should be readable");
    assert!(!contains_bytes(&database, b"not-a-real-search-key"));
}

#[tokio::test(flavor = "current_thread")]
async fn ipython_tool_reuses_one_session_kernel_and_commits_bounded_results() {
    let root = TestRoot::new("ipython-tool-loop");
    let selected = TestRoot::new("ipython-directory");
    let store = SessionStore::open_for_test(root.path()).expect("session store should open");
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0xa1; 16]),
            0,
            b"not-a-real-ipython-key".to_vec(),
        )
        .await
        .expect("credential should be configured");
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0xa2; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let (provider_base, provider_requests, provider_task) =
        spawn_ipython_tool_loop_provider().await;
    let application =
        ServerApplication::from_session_store_with_ipython_for_test(store, &provider_base);
    let session_id = SessionId::from_bytes(*session.id.as_bytes());
    let accepted = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0xa3; 16]),
            session_id,
            text: "use persistent Python state".to_owned(),
            attachments: Vec::new(),
            service: ModelService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .expect("IPython run should be accepted");
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        accepted
    else {
        panic!("input should return a run");
    };
    assert_eq!(
        wait_for_terminal(&application, session_id, run.id).await,
        RunState::Succeeded
    );
    provider_task.await.expect("provider fixture should finish");
    let provider_requests = provider_requests
        .await
        .expect("provider requests should be captured");
    assert_eq!(provider_requests.len(), 3);
    assert!(provider_requests[0].contains("\"name\":\"ipython\""));
    assert!(provider_requests[1].contains("\\\"execution_count\\\":1"));
    assert!(provider_requests[2].contains("\\\"display\\\":\\\"42\\\""));

    let mut cursor = None;
    let mut entries = Vec::new();
    loop {
        let outcome = application
            .execute_for_local_owner(ApplicationRequest::ListSessionTranscript {
                session_id,
                cursor,
                direction: morons_protocol::TranscriptPageDirection::Newer,
                limit: 1,
            })
            .await
            .expect("transcript should load");
        let ApplicationOutcome::Response(ApplicationResponse::SessionTranscriptListed {
            entries: page,
            newer_cursor,
            ..
        }) = outcome
        else {
            panic!("transcript should return a page");
        };
        entries.extend(page);
        let Some(next) = newer_cursor else { break };
        cursor = Some(next);
    }
    for index in [1, 3] {
        assert!(matches!(
            entries[index],
            morons_protocol::TranscriptEntry::ToolCall {
                tool: morons_protocol::ToolKind::Ipython,
                ..
            }
        ));
    }
    for index in [2, 4] {
        assert!(matches!(
            entries[index],
            morons_protocol::TranscriptEntry::ToolResult {
                status: morons_protocol::ToolResultStatus::Succeeded,
                ..
            }
        ));
    }
    application.shutdown().await;
    drop(application);
    SessionStore::open_for_test(root.path()).expect("IPython history should reopen");
}
