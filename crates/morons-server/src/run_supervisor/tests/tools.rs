use super::*;

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
            service: OpenCodeService::Zen,
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
            service: OpenCodeService::Zen,
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
            service: OpenCodeService::Zen,
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
    assert!(search_request.starts_with(
        "GET /search?q=current%20Rust%20release&count=10&safesearch=moderate&spellcheck=1 HTTP/1.1"
    ));
    let provider_requests = provider_requests
        .await
        .expect("provider requests should be captured");
    assert_eq!(provider_requests.len(), 2);
    assert!(provider_requests[0].contains("\"name\":\"web_search\""));
    assert!(provider_requests[1].contains("https://www.rust-lang.org/"));
    assert!(provider_requests[1].contains("Rust is a programming language"));
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
    application.shutdown().await;
    drop(application);
    SessionStore::open_for_test(root.path()).expect("web search history should reopen");
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
            service: OpenCodeService::Zen,
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
