use std::{fs, path::PathBuf, process, sync::Arc, time::Duration};

use morons_cli::ApplicationClient;

use morons_protocol::{
    ApplicationError, ApplicationEvent, ApplicationRequest, ApplicationResponse, MutationRequestId,
    OpenCodeService, RunId, RunState, SessionId,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    sync::oneshot,
    time,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::{RunSupervisor, build_provider_request, completed_assistant};
use crate::{
    application::{ApplicationOutcome, ServerApplication, events::SessionEventHub},
    handle_local_owner_requests,
    persistence::{
        ActivationOutcome, MAX_TRANSCRIPT_TEXT_BYTES,
        MutationRequestId as PersistenceMutationRequestId, PrepareOperationOutcome,
        ProviderOperationFailureState, RunFailureKind, RunModelSelection, RunOpenCodeService,
        RunState as PersistenceRunState, SessionStore,
    },
    provider::{
        OpenCodeProvider, ProviderAssistantMessage, ProviderError, ProviderMessagePhase,
        ProviderOutcome, ProviderOutputItem, ProviderToolCall, ProviderUsage,
    },
};

#[test]
fn global_run_capacity_is_bounded_without_queueing() {
    let root = TestRoot::new("global-capacity");
    let sessions =
        Arc::new(SessionStore::open_for_test(root.path()).expect("session store should open"));
    let provider = Arc::new(OpenCodeProvider::for_test(
        Arc::clone(&sessions),
        "http://127.0.0.1:9",
    ));
    let supervisor = RunSupervisor::new(sessions, provider, SessionEventHub::new());
    let permits = (0..4)
        .map(|_| supervisor.try_reserve().expect("capacity should remain"))
        .collect::<Vec<_>>();
    assert!(supervisor.try_reserve().is_none());
    drop(permits);
    assert!(supervisor.try_reserve().is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn model_catalog_query_returns_only_reviewed_server_metadata() {
    let root = TestRoot::new("model-catalog-query");
    let store = SessionStore::open_for_test(root.path()).expect("session store should open");
    let (base, captured_request, server) = spawn_catalog_provider().await;
    let application = ServerApplication::from_session_store_for_test(store, &base);

    let outcome = application
        .execute_for_local_owner(ApplicationRequest::ListOpenCodeModels {
            service: OpenCodeService::Go,
        })
        .await
        .expect("model catalog query should succeed");
    let ApplicationOutcome::Response(ApplicationResponse::OpenCodeModelsListed { service, models }) =
        outcome
    else {
        panic!("model catalog query should return model summaries");
    };
    assert_eq!(service, OpenCodeService::Go);
    assert!(models.iter().all(|model| model.service == service));
    assert!(
        models
            .iter()
            .find(|model| model.id == "gpt-5.6-luna")
            .expect("reviewed model should be returned")
            .available
    );
    assert!(
        models
            .iter()
            .all(|model| model.id != "muse-spark-1.2-contributor")
    );

    let captured = captured_request
        .await
        .expect("catalog request should be captured");
    assert!(captured.starts_with("GET /zen/go/v1/models HTTP/1.1"));
    assert!(!captured.to_ascii_lowercase().contains("authorization:"));
    server.await.expect("catalog fixture should finish");
    application.shutdown().await;
}

#[test]
fn tool_turn_validation_rejects_the_entire_unknown_or_contradictory_response() {
    let usage = ProviderUsage {
        input_tokens: 1,
        cached_input_tokens: 0,
        cache_write_input_tokens: 0,
        output_tokens: 1,
        reasoning_output_tokens: 0,
        total_tokens: 2,
    };
    let unknown = ProviderOutcome {
        provider_response_id: "resp_unknown_tool".to_owned(),
        output: vec![ProviderOutputItem::ToolCall(ProviderToolCall {
            provider_item_id: Some("fc_unknown".to_owned()),
            provider_call_id: "call_unknown".to_owned(),
            name: "unknown_tool".to_owned(),
            arguments: "{}".to_owned(),
        })],
        usage,
    };
    assert!(matches!(
        super::normalize_provider_turn(unknown, crate::tools::TOOL_CATALOG_VERSION),
        Err(RunFailureKind::InvalidProviderOutput)
    ));

    let contradictory = ProviderOutcome {
        provider_response_id: "resp_contradictory".to_owned(),
        output: vec![
            ProviderOutputItem::AssistantMessage(ProviderAssistantMessage {
                provider_item_id: "msg_final".to_owned(),
                phase: Some(ProviderMessagePhase::FinalAnswer),
                text: "done".to_owned(),
                refusal: false,
            }),
            ProviderOutputItem::ToolCall(ProviderToolCall {
                provider_item_id: Some("fc_read".to_owned()),
                provider_call_id: "call_read".to_owned(),
                name: "read_file".to_owned(),
                arguments: r#"{"path":"note.txt","start_line":1,"line_count":1}"#.to_owned(),
            }),
        ],
        usage,
    };
    assert!(matches!(
        super::normalize_provider_turn(contradictory, crate::tools::TOOL_CATALOG_VERSION),
        Err(RunFailureKind::InvalidProviderOutput)
    ));
}

#[test]
fn nonvision_models_reject_read_image_results_before_persistence() {
    let image =
        morons_image::normalize_rgba(1, 1, vec![1, 2, 3, 255]).expect("fixture should normalize");
    let result = crate::tools::ToolResult::Ok {
        output: crate::tools::ToolOutput::ReadImage {
            path: crate::tools::ToolPath::parse("picture.png").expect("path should parse"),
            image: crate::tools::ToolImageOutput {
                attachment_id: None,
                display_name: "picture.png".to_owned(),
                media_type: image.media_type,
                width: image.width,
                height: image.height,
                bytes: image.bytes.len() as u64,
                sha256: "00".repeat(32),
                data: image.bytes,
            },
        },
    };
    assert_eq!(
        super::enforce_image_capability(result, false),
        crate::tools::ToolResult::error(crate::tools::ToolErrorKind::ImageInputUnsupported)
    );
}

#[test]
fn oversized_complete_assistant_is_a_run_resource_failure() {
    let outcome = ProviderOutcome {
        provider_response_id: "resp_oversized".to_owned(),
        output: vec![ProviderOutputItem::AssistantMessage(
            ProviderAssistantMessage {
                provider_item_id: "msg_oversized".to_owned(),
                phase: None,
                text: "x".repeat(MAX_TRANSCRIPT_TEXT_BYTES + 1),
                refusal: false,
            },
        )],
        usage: ProviderUsage {
            input_tokens: 1,
            cached_input_tokens: 0,
            cache_write_input_tokens: 0,
            output_tokens: 1,
            reasoning_output_tokens: 0,
            total_tokens: 2,
        },
    };
    assert_eq!(
        completed_assistant(outcome).expect_err("oversized assistant should fail"),
        RunFailureKind::ResourceLimit
    );
}

#[tokio::test(flavor = "current_thread")]
async fn changed_credential_generation_fails_before_network_dispatch() {
    let root = TestRoot::new("credential-generation");
    let store = SessionStore::open_for_test(root.path()).expect("session store should open");
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0x71; 16]),
            0,
            b"not-a-real-original-key".to_vec(),
        )
        .await
        .expect("original credential should be configured");
    let session = store
        .create_session(PersistenceMutationRequestId::from_bytes([0x72; 16]), None)
        .await
        .expect("session should be created");
    let accepted = store
        .accept_session_input(
            PersistenceMutationRequestId::from_bytes([0x73; 16]),
            session.id,
            "bind the original generation".to_owned(),
            RunModelSelection {
                service: RunOpenCodeService::Zen,
                model_id: "muse-spark-1.2".to_owned(),
                protocol_revision: 1,
                maximum_input_tokens: 96_000,
                maximum_output_tokens: 32_000,
                supports_tool_calls: true,
                supports_image_input: false,
            },
        )
        .await
        .expect("run should bind generation one");
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0x74; 16]),
            1,
            b"not-a-real-replacement-key".to_vec(),
        )
        .await
        .expect("credential should be replaced");
    assert_eq!(
        store
            .activate_run(accepted.run.id)
            .await
            .expect("run should activate"),
        ActivationOutcome::Active
    );
    let context = store
        .load_run_context(accepted.run.id)
        .await
        .expect("run context should load");
    let request = build_provider_request(&context, None).expect("request should build");
    let operation_id = match store
        .prepare_provider_operation(
            accepted.run.id,
            context.current_entry_high_water,
            context.estimated_input_tokens,
        )
        .await
        .expect("provider operation should prepare")
    {
        PrepareOperationOutcome::Prepared(operation_id) => operation_id,
        other => panic!("unexpected preparation outcome: {other:?}"),
    };
    let sessions = Arc::new(store);
    let provider = OpenCodeProvider::for_test(Arc::clone(&sessions), "http://127.0.0.1:9");
    let error = match provider
        .prepare_dispatch(accepted.run.credential_generation, &request)
        .await
    {
        Ok(_) => panic!("changed generation must not prepare network dispatch"),
        Err(error) => error,
    };
    assert_eq!(error, ProviderError::CredentialGenerationChanged);
    let failed = sessions
        .finish_run_failure(
            accepted.run.id,
            Some(operation_id),
            RunFailureKind::CredentialChanged,
            ProviderOperationFailureState::Failed,
        )
        .await
        .expect("run should fail durably");
    assert_eq!(failed.state, PersistenceRunState::Failed);
}

#[tokio::test(flavor = "current_thread")]
async fn accepted_run_outlives_request_and_commits_complete_assistant() {
    let root = TestRoot::new("supervised-success");
    let store = SessionStore::open_for_test(root.path()).expect("session store should open");
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0x01; 16]),
            0,
            b"not-a-real-supervisor-key".to_vec(),
        )
        .await
        .expect("credential should be configured");
    let session = store
        .create_session(PersistenceMutationRequestId::from_bytes([0x02; 16]), None)
        .await
        .expect("session should be created");
    let (base, captured_request, complete_provider, server) = spawn_successful_provider().await;
    let application = Arc::new(ServerApplication::from_session_store_for_test(store, &base));
    let protocol_session_id = SessionId::from_bytes(*session.id.as_bytes());
    let snapshot = application
        .execute_for_local_owner(ApplicationRequest::ListSessionTranscript {
            session_id: protocol_session_id,
            cursor: None,
            limit: 1,
        })
        .await
        .expect("initial session snapshot should load");
    let ApplicationOutcome::Response(ApplicationResponse::SessionTranscriptListed {
        event_cursor,
        ..
    }) = snapshot
    else {
        panic!("initial session snapshot should return a page");
    };

    let (subscription_connection, mut subscription_server_connection) =
        tokio::io::duplex(1024 * 1024);
    let subscription_application = Arc::clone(&application);
    let subscription_requests = tokio::spawn(async move {
        handle_local_owner_requests(
            &mut subscription_server_connection,
            &subscription_application,
        )
        .await
    });
    let subscription_client =
        ApplicationClient::from_negotiated_connection(subscription_connection);
    let mut subscription = subscription_client
        .subscribe_to_session(protocol_session_id, event_cursor)
        .await
        .expect("session subscription should start");

    let (client_connection, mut server_connection) = tokio::io::duplex(1024 * 1024);
    let server_application = Arc::clone(&application);
    let requests = tokio::spawn(async move {
        handle_local_owner_requests(&mut server_connection, &server_application).await
    });
    let mut client = ApplicationClient::from_negotiated_connection(client_connection);
    let accepted = client
        .submit_session_input(
            MutationRequestId::from_bytes([0x03; 16]),
            protocol_session_id,
            "return a durable answer".to_owned(),
            OpenCodeService::Zen,
            "muse-spark-1.2".to_owned(),
        )
        .await
        .expect("input should be accepted over the application transport");
    let run = accepted.run;
    drop(client);
    requests
        .await
        .expect("request task should join")
        .expect("client disconnect should close the request loop cleanly");

    let captured = time::timeout(Duration::from_secs(5), captured_request)
        .await
        .expect("provider request should be dispatched")
        .expect("provider request capture should complete");
    assert!(captured.contains("POST /zen/v1/responses HTTP/1.1"));
    assert!(captured.contains("authorization: Bearer not-a-real-supervisor-key"));
    let mut saw_user = false;
    let mut saw_accepted = false;
    let mut saw_active = false;
    loop {
        let event = time::timeout(Duration::from_secs(5), subscription.next_event())
            .await
            .expect("live session event should arrive")
            .expect("live session event should be valid");
        match event {
            ApplicationEvent::SessionTranscriptEntryCommitted {
                entry: morons_protocol::TranscriptEntry::UserMessage { run_id, .. },
                ..
            } if run_id == run.id => saw_user = true,
            ApplicationEvent::SessionRunChanged { run: changed, .. }
                if changed.id == run.id && changed.state == RunState::Accepted =>
            {
                saw_accepted = true;
            }
            ApplicationEvent::SessionRunChanged { run: changed, .. }
                if changed.id == run.id && changed.state == RunState::Active =>
            {
                saw_active = true;
            }
            ApplicationEvent::SessionAssistantDelta {
                run_id,
                sequence: 1,
                delta,
                ..
            } if run_id == run.id && delta == "durable answer" => break,
            other => panic!("unexpected live session event: {other:?}"),
        }
    }
    assert!(saw_user && saw_accepted && saw_active);
    complete_provider
        .send(())
        .unwrap_or_else(|_| panic!("provider completion should be released"));
    server.await.expect("provider fixture should finish");

    let mut saw_assistant = false;
    let terminal = loop {
        let event = time::timeout(Duration::from_secs(5), subscription.next_event())
            .await
            .expect("terminal session event should arrive")
            .expect("terminal session event should be valid");
        match event {
            ApplicationEvent::SessionTranscriptEntryCommitted {
                entry: morons_protocol::TranscriptEntry::AssistantMessage { text, .. },
                ..
            } if text == "durable answer" => saw_assistant = true,
            ApplicationEvent::SessionRunChanged { run: changed, .. }
                if changed.id == run.id && changed.state.is_terminal() =>
            {
                break changed.state;
            }
            other => panic!("unexpected terminal session event: {other:?}"),
        }
    };
    assert!(saw_assistant);
    assert_eq!(terminal, RunState::Succeeded);
    let first = application
        .execute_for_local_owner(ApplicationRequest::ListSessionTranscript {
            session_id: run.session_id,
            cursor: None,
            limit: 1,
        })
        .await
        .expect("first transcript page should load");
    let ApplicationOutcome::Response(ApplicationResponse::SessionTranscriptListed {
        entries,
        next_cursor,
        ..
    }) = first
    else {
        panic!("transcript should return a page");
    };
    assert_eq!(entries.len(), 1);
    let second = application
        .execute_for_local_owner(ApplicationRequest::ListSessionTranscript {
            session_id: run.session_id,
            cursor: next_cursor,
            limit: 1,
        })
        .await
        .expect("second transcript page should load");
    let ApplicationOutcome::Response(ApplicationResponse::SessionTranscriptListed {
        entries,
        next_cursor,
        ..
    }) = second
    else {
        panic!("transcript should return a page");
    };
    assert!(next_cursor.is_none());
    assert!(matches!(
        &entries[..],
        [morons_protocol::TranscriptEntry::AssistantMessage { text, .. }]
            if text == "durable answer"
    ));
    drop(subscription);
    subscription_requests
        .await
        .expect("subscription task should join")
        .expect("subscription disconnect should be clean");
    application.shutdown().await;
    drop(application);
    let database = fs::read(root.path().join("data").join("sessions.sqlite3"))
        .expect("database should be readable");
    assert!(!contains_bytes(&database, b"not-a-real-supervisor-key"));
    assert!(!contains_bytes(&database, b"response.completed"));
    assert!(!contains_bytes(&database, b"Bearer "));
}

#[tokio::test(flavor = "current_thread")]
async fn manual_compaction_runs_below_threshold_with_bounded_user_guidance() {
    let root = TestRoot::new("manual-context-compaction");
    let selected = TestRoot::new("manual-context-compaction-directory");
    let store = SessionStore::open_for_test(root.path()).expect("session store should open");
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0xd1; 16]),
            0,
            b"not-a-real-manual-compaction-key".to_vec(),
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
    append_completed_context_run(&store, session.id, 0xd3, "OLD_MANUAL_ONE", 100).await;
    append_completed_context_run(&store, session.id, 0xd4, "RECENT_MANUAL_TWO", 100).await;
    append_completed_context_run(&store, session.id, 0xd5, "RECENT_MANUAL_THREE", 100).await;
    let (base, requests, provider_task) = spawn_compaction_provider().await;
    let application = ServerApplication::from_session_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.id.as_bytes());
    let before = application
        .execute_for_local_owner(ApplicationRequest::GetSessionContext {
            session_id,
            service: OpenCodeService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .expect("context status should load");
    let ApplicationOutcome::Response(ApplicationResponse::SessionContextFound { context: before }) =
        before
    else {
        panic!("context status should be returned");
    };
    assert!(before.estimated_input_tokens < before.compaction_threshold_tokens);
    assert_eq!(before.checkpoint_source_entry_high_water, None);

    let accepted = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0xd6; 16]),
            session_id,
            text: "/compact preserve the frog migration decision".to_owned(),
            attachments: Vec::new(),
            service: OpenCodeService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .expect("manual compaction should be accepted");
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        accepted
    else {
        panic!("manual compaction should return a run");
    };
    assert_eq!(
        wait_for_terminal(&application, session_id, run.id).await,
        RunState::Succeeded
    );
    provider_task.await.expect("provider should finish");
    let requests = requests.await.expect("requests should be captured");
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("Untrusted user-requested summary emphasis"));
    assert!(requests[0].contains("preserve the frog migration decision"));
    assert!(requests[0].contains("OLD_MANUAL_ONE"));
    assert!(!requests[0].contains("RECENT_MANUAL_TWO"));

    let after = application
        .execute_for_local_owner(ApplicationRequest::GetSessionContext {
            session_id,
            service: OpenCodeService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .expect("compacted context status should load");
    let ApplicationOutcome::Response(ApplicationResponse::SessionContextFound { context: after }) =
        after
    else {
        panic!("context status should be returned");
    };
    assert!(after.checkpoint_source_entry_high_water.is_some());
    assert!(after.checkpoint_estimated_summary_tokens.is_some());
    application.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn proactive_compaction_commits_a_source_bound_summary_and_uses_recent_tail() {
    let root = TestRoot::new("context-compaction");
    let selected = TestRoot::new("context-compaction-directory");
    let store = SessionStore::open_for_test(root.path()).expect("session store should open");
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0xe1; 16]),
            0,
            b"not-a-real-compaction-key".to_vec(),
        )
        .await
        .expect("credential should be configured");
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0xe2; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    append_completed_context_run(&store, session.id, 0xe3, "OLD_RUN_ONE", 24_000).await;
    let hidden = store
        .accept_local_command(
            PersistenceMutationRequestId::from_bytes([0xe7; 16]),
            session.id,
            "SECRET_CONTEXT_EXCLUDED".to_owned(),
            false,
        )
        .await
        .expect("context-excluded command should be accepted");
    assert!(
        store
            .activate_local_command(hidden.id)
            .await
            .expect("context-excluded command should activate")
    );
    store
        .complete_local_command(
            hidden.id,
            crate::tools::ToolResult::Ok {
                output: crate::tools::ToolOutput::Bash {
                    exit_code: Some(0),
                    signal: None,
                    stdout: "SECRET_CONTEXT_EXCLUDED_OUTPUT".to_owned(),
                    stderr: String::new(),
                },
            },
        )
        .await
        .expect("context-excluded command should complete");
    append_completed_context_run(&store, session.id, 0xe4, "RECENT_RUN_TWO", 24_000).await;
    append_completed_context_run(&store, session.id, 0xe5, "RECENT_RUN_THREE", 24_000).await;
    let (base, requests, provider_task) = spawn_compaction_provider().await;
    let application = ServerApplication::from_session_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.id.as_bytes());
    let accepted = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0xe6; 16]),
            session_id,
            text: "CURRENT_RUN_FOUR".to_owned(),
            attachments: Vec::new(),
            service: OpenCodeService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .expect("compacting run should be accepted");
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        accepted
    else {
        panic!("input should return a run");
    };
    assert_eq!(
        wait_for_terminal(&application, session_id, run.id).await,
        RunState::Succeeded
    );
    provider_task
        .await
        .expect("compaction provider should finish");
    let requests = requests.await.expect("requests should be captured");
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("Summarize the supplied earlier session prefix"));
    assert!(requests[0].contains("OLD_RUN_ONE"));
    assert!(!requests[0].contains("SECRET_CONTEXT_EXCLUDED"));
    assert!(!requests[0].contains("CURRENT_RUN_FOUR"));
    assert!(requests[1].contains("SUMMARY_OF_OLDEST_PREFIX"));
    assert!(!requests[1].contains("OLD_RUN_ONE"));
    assert!(requests[1].contains("RECENT_RUN_TWO"));
    assert!(requests[1].contains("RECENT_RUN_THREE"));
    assert!(requests[1].contains("CURRENT_RUN_FOUR"));
    application.shutdown().await;
    drop(application);
    let reopened =
        SessionStore::open_for_test(root.path()).expect("compaction checkpoint should reopen");
    drop(reopened);
    let connection = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3"))
        .expect("database should open for corruption");
    connection
        .execute(
            "UPDATE context_checkpoints SET source_digest = ?1",
            [&[0_u8; 32][..]],
        )
        .expect("checkpoint digest should be corrupted");
    drop(connection);
    assert!(SessionStore::open_for_test(root.path()).is_err());
}

async fn append_completed_context_run(
    store: &SessionStore,
    session_id: crate::persistence::SessionId,
    request_byte: u8,
    marker: &str,
    assistant_padding: usize,
) {
    let accepted = store
        .accept_session_input(
            PersistenceMutationRequestId::from_bytes([request_byte; 16]),
            session_id,
            marker.to_owned(),
            RunModelSelection {
                service: RunOpenCodeService::Zen,
                model_id: "muse-spark-1.2".to_owned(),
                protocol_revision: 1,
                maximum_input_tokens: 96_000,
                maximum_output_tokens: 32_000,
                supports_tool_calls: true,
                supports_image_input: false,
            },
        )
        .await
        .expect("context fixture run should be accepted");
    assert_eq!(
        store
            .activate_run(accepted.run.id)
            .await
            .expect("fixture run should activate"),
        ActivationOutcome::Active
    );
    let context = store
        .load_run_context(accepted.run.id)
        .await
        .expect("fixture context should load");
    let operation = match store
        .prepare_provider_operation(
            accepted.run.id,
            context.current_entry_high_water,
            context.estimated_input_tokens,
        )
        .await
        .expect("fixture operation should prepare")
    {
        PrepareOperationOutcome::Prepared(operation) => operation,
        other => panic!("unexpected preparation outcome: {other:?}"),
    };
    assert!(matches!(
        store
            .mark_provider_dispatched(accepted.run.id, operation)
            .await
            .expect("fixture operation should dispatch"),
        crate::persistence::DispatchOutcome::Dispatched
    ));
    store
        .complete_run_success(
            accepted.run.id,
            operation,
            crate::persistence::CompletedAssistant {
                text: format!("{marker}_ASSISTANT {}", "x".repeat(assistant_padding)),
                refusal: false,
                provider_response_id: format!("resp_{marker}"),
                usage: crate::persistence::ProviderUsage {
                    input_tokens: 10,
                    cached_input_tokens: 0,
                    cache_write_input_tokens: 0,
                    output_tokens: 10,
                    reasoning_output_tokens: 0,
                    total_tokens: 20,
                },
            },
        )
        .await
        .expect("fixture run should complete");
}

#[tokio::test(flavor = "current_thread")]
async fn image_submission_requires_vision_and_maps_durable_bytes_to_multimodal_content() {
    let root = TestRoot::new("image-context");
    let selected = TestRoot::new("image-directory");
    let store = SessionStore::open_for_test(root.path()).expect("session store should open");
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0xc1; 16]),
            0,
            b"not-a-real-image-key".to_vec(),
        )
        .await
        .expect("credential should be configured");
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0xc2; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let image =
        morons_image::normalize_rgba(2, 2, vec![0x88; 16]).expect("fixture image should normalize");
    let upload = morons_protocol::ImageUpload {
        display_name: "puppies.png".to_owned(),
        marker_start: 4,
        data_base64: morons_image::encode_base64(&image.bytes),
    };
    let (base, captured_request, provider_task) = spawn_image_provider().await;
    let application = ServerApplication::from_session_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.id.as_bytes());

    let unsupported = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0xc3; 16]),
            session_id,
            text: "see [puppies.png]".to_owned(),
            attachments: vec![upload.clone()],
            service: OpenCodeService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await;
    assert!(matches!(
        unsupported,
        Err(ApplicationError::UnsupportedModel)
    ));
    assert_eq!(
        fs::read_dir(root.path().join("attachments"))
            .expect("attachment directory should be readable")
            .count(),
        0
    );

    let accepted = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0xc4; 16]),
            session_id,
            text: "see [puppies.png]".to_owned(),
            attachments: vec![upload],
            service: OpenCodeService::Zen,
            model_id: "gpt-5.4".to_owned(),
        })
        .await
        .expect("vision run should be accepted");
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        accepted
    else {
        panic!("input should return a run");
    };
    assert_eq!(
        wait_for_terminal(&application, session_id, run.id).await,
        RunState::Succeeded
    );
    provider_task.await.expect("image provider should finish");
    let request = captured_request
        .await
        .expect("image request should be captured");
    assert!(request.contains("\"type\":\"input_text\""));
    assert!(request.contains("\"type\":\"input_image\""));
    assert!(request.contains("data:image/png;base64,"));
    assert!(request.contains("[puppies.png]"));

    let page = application
        .execute_for_local_owner(ApplicationRequest::ListSessionTranscript {
            session_id,
            cursor: None,
            limit: 1,
        })
        .await
        .expect("image transcript should load");
    let ApplicationOutcome::Response(ApplicationResponse::SessionTranscriptListed {
        entries, ..
    }) = page
    else {
        panic!("transcript should return a page");
    };
    assert!(matches!(
        &entries[..],
        [morons_protocol::TranscriptEntry::UserMessage { attachments, .. }]
            if attachments.len() == 1 && attachments[0].display_name == "puppies.png"
    ));
    application.shutdown().await;
    drop(application);
    let database =
        fs::read(root.path().join("data/sessions.sqlite3")).expect("database should be readable");
    assert!(!contains_bytes(&database, &image.bytes));
    assert!(!contains_bytes(
        &database,
        morons_image::encode_base64(&image.bytes).as_bytes()
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn exact_skill_invocation_binds_full_instructions_while_catalog_stays_progressive() {
    let root = TestRoot::new("skill-context");
    let selected = TestRoot::new("skill-directory");
    write_test_skill(
        selected.path(),
        "release-helper",
        "Prepares releases when the user asks for release work.",
        "ACTIVE_RELEASE_INSTRUCTIONS",
    );
    write_test_skill(
        selected.path(),
        "inactive-helper",
        "Handles inactive test work.",
        "INACTIVE_PRIVATE_INSTRUCTIONS",
    );
    let store = SessionStore::open_for_test(root.path()).expect("session store should open");
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0xb1; 16]),
            0,
            b"not-a-real-skill-key".to_vec(),
        )
        .await
        .expect("credential should be configured");
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0xb2; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let (base, captured_request, complete_provider, provider_task) =
        spawn_successful_provider().await;
    let application = ServerApplication::from_session_store_for_test(store, &base);
    let protocol_session_id = SessionId::from_bytes(*session.id.as_bytes());
    let catalog = application
        .execute_for_local_owner(ApplicationRequest::ListSessionSkills {
            session_id: protocol_session_id,
        })
        .await
        .expect("skill catalog should load");
    let ApplicationOutcome::Response(ApplicationResponse::SessionSkillsListed {
        skills,
        warnings,
        ..
    }) = catalog
    else {
        panic!("skill catalog should return a response");
    };
    assert!(warnings.is_empty());
    assert_eq!(
        skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>(),
        ["inactive-helper", "release-helper", "skill-creator"]
    );
    let accepted = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0xb3; 16]),
            session_id: protocol_session_id,
            text: "@release-helper prepare a release".to_owned(),
            attachments: Vec::new(),
            service: OpenCodeService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .expect("skill-bearing input should be accepted");
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        accepted
    else {
        panic!("input should return a run");
    };
    let request = time::timeout(Duration::from_secs(5), captured_request)
        .await
        .expect("provider request should dispatch")
        .expect("provider request should be captured");
    assert!(request.contains("Prepares releases when the user asks for release work."));
    assert!(request.contains("Handles inactive test work."));
    assert!(request.contains("ACTIVE_RELEASE_INSTRUCTIONS"));
    assert!(!request.contains("INACTIVE_PRIVATE_INSTRUCTIONS"));
    complete_provider
        .send(())
        .unwrap_or_else(|_| panic!("provider completion should be released"));
    provider_task.await.expect("provider fixture should finish");
    assert_eq!(
        wait_for_terminal(&application, run.session_id, run.id).await,
        RunState::Succeeded
    );
    application.shutdown().await;
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
                limit: 1,
            })
            .await
            .expect("tool transcript should page");
        let ApplicationOutcome::Response(ApplicationResponse::SessionTranscriptListed {
            entries: page,
            next_cursor,
            ..
        }) = outcome
        else {
            panic!("transcript should return a page");
        };
        entries.extend(page);
        let Some(next) = next_cursor else { break };
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
                limit: 1,
            })
            .await
            .expect("transcript should load");
        let ApplicationOutcome::Response(ApplicationResponse::SessionTranscriptListed {
            entries: page,
            next_cursor,
            ..
        }) = outcome
        else {
            panic!("transcript should return a page");
        };
        entries.extend(page);
        let Some(next) = next_cursor else { break };
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
                limit: 1,
            })
            .await
            .expect("transcript should load");
        let ApplicationOutcome::Response(ApplicationResponse::SessionTranscriptListed {
            entries: page,
            next_cursor,
            ..
        }) = outcome
        else {
            panic!("transcript should return a page");
        };
        entries.extend(page);
        let Some(next) = next_cursor else { break };
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

#[tokio::test(flavor = "current_thread")]
async fn exact_cancellation_stops_the_supervised_provider_task() {
    let root = TestRoot::new("supervised-cancellation");
    let store = SessionStore::open_for_test(root.path()).expect("session store should open");
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0x11; 16]),
            0,
            b"not-a-real-cancellation-key".to_vec(),
        )
        .await
        .expect("credential should be configured");
    let session = store
        .create_session(PersistenceMutationRequestId::from_bytes([0x12; 16]), None)
        .await
        .expect("session should be created");
    let (base, dispatched, server) = spawn_stalled_provider().await;
    let application = ServerApplication::from_session_store_for_test(store, &base);
    let accepted = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0x13; 16]),
            session_id: SessionId::from_bytes(*session.id.as_bytes()),
            text: "cancel the network request".to_owned(),
            attachments: Vec::new(),
            service: OpenCodeService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .expect("input should be accepted");
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        accepted
    else {
        panic!("input should return a run acceptance");
    };
    time::timeout(Duration::from_secs(5), dispatched)
        .await
        .expect("provider request should dispatch")
        .expect("dispatch signal should arrive");

    let cancellation = application
        .execute_for_local_owner(ApplicationRequest::CancelRun {
            mutation_request_id: MutationRequestId::from_bytes([0x14; 16]),
            session_id: run.session_id,
            run_id: run.id,
        })
        .await
        .expect("cancellation should commit");
    assert!(matches!(
        cancellation,
        ApplicationOutcome::Response(ApplicationResponse::RunCancellationResolved {
            run_id,
            cancellation_requested: true,
            ..
        }) if run_id == run.id
    ));
    assert_eq!(
        wait_for_terminal(&application, run.session_id, run.id).await,
        RunState::Cancelled
    );
    server
        .await
        .expect("stalled provider should observe closure");
    application.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn graceful_shutdown_interrupts_run_without_owner_cancellation() {
    let root = TestRoot::new("supervised-shutdown");
    let store = SessionStore::open_for_test(root.path()).expect("session store should open");
    store
        .set_open_code_credential(
            PersistenceMutationRequestId::from_bytes([0x21; 16]),
            0,
            b"not-a-real-shutdown-key".to_vec(),
        )
        .await
        .expect("credential should be configured");
    let session = store
        .create_session(PersistenceMutationRequestId::from_bytes([0x22; 16]), None)
        .await
        .expect("session should be created");
    let (base, dispatched, server) = spawn_stalled_provider().await;
    let application = ServerApplication::from_session_store_for_test(store, &base);
    let accepted = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0x23; 16]),
            session_id: SessionId::from_bytes(*session.id.as_bytes()),
            text: "interrupt on shutdown".to_owned(),
            attachments: Vec::new(),
            service: OpenCodeService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
        .expect("input should be accepted");
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        accepted
    else {
        panic!("input should return a run acceptance");
    };
    time::timeout(Duration::from_secs(5), dispatched)
        .await
        .expect("provider request should dispatch")
        .expect("dispatch signal should arrive");

    application.shutdown().await;
    server.await.expect("provider should observe shutdown");
    let outcome = application
        .execute_for_local_owner(ApplicationRequest::GetRun {
            session_id: run.session_id,
            run_id: run.id,
        })
        .await
        .expect("interrupted run should remain queryable");
    assert!(matches!(
        outcome,
        ApplicationOutcome::Response(ApplicationResponse::RunFound { run })
            if run.state == RunState::Interrupted
    ));
    let error = match application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0x24; 16]),
            session_id: run.session_id,
            text: "must not start during shutdown".to_owned(),
            attachments: Vec::new(),
            service: OpenCodeService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await
    {
        Ok(_) => panic!("shutdown should reject new run input"),
        Err(error) => error,
    };
    assert_eq!(error, ApplicationError::ServiceUnavailable);
}

async fn wait_for_terminal(
    application: &ServerApplication,
    session_id: SessionId,
    run_id: RunId,
) -> RunState {
    time::timeout(Duration::from_secs(15), async {
        loop {
            let outcome = application
                .execute_for_local_owner(ApplicationRequest::GetRun { session_id, run_id })
                .await
                .expect("run query should succeed");
            let ApplicationOutcome::Response(ApplicationResponse::RunFound { run }) = outcome
            else {
                panic!("run query should return a run");
            };
            if run.state.is_terminal() {
                return run.state;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("run should terminate")
}

async fn spawn_catalog_provider() -> (
    String,
    oneshot::Receiver<String>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("catalog fixture should bind");
    let address = listener
        .local_addr()
        .expect("catalog fixture should have an address");
    let (captured_sender, captured_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("catalog should connect");
        let request = read_http_request(&mut stream).await;
        captured_sender
            .send(String::from_utf8(request).expect("catalog request should be UTF-8"))
            .unwrap_or_else(|_| panic!("catalog request should be observed"));
        let body = concat!(
            "{\"object\":\"list\",\"data\":[",
            "{\"id\":\"gpt-5.6-luna\",\"object\":\"model\",\"created\":1,\"owned_by\":\"opencode\"},",
            "{\"id\":\"muse-spark-1.2-contributor\",\"object\":\"model\",\"created\":1,\"owned_by\":\"opencode\"}",
            "]}"
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("catalog response should be written");
        stream
            .shutdown()
            .await
            .expect("catalog response should close");
    });
    (format!("http://{address}"), captured_receiver, server)
}

async fn spawn_compaction_provider() -> (
    String,
    oneshot::Receiver<Vec<String>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("compaction provider should bind");
    let address = listener.local_addr().expect("provider address should load");
    let (requests_sender, requests_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let answers = ["SUMMARY_OF_OLDEST_PREFIX", "compacted answer"];
        let mut captured = Vec::new();
        for (index, answer) in answers.into_iter().enumerate() {
            let (mut stream, _) = listener.accept().await.expect("provider should connect");
            captured.push(
                String::from_utf8(read_http_request(&mut stream).await)
                    .expect("request should be UTF-8"),
            );
            let response_id = format!("resp_compact_{}", index + 1);
            let message_id = format!("msg_compact_{}", index + 1);
            let output = format!(
                "{{\"id\":\"{message_id}\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"phase\":\"final_answer\",\"content\":[{{\"type\":\"output_text\",\"text\":\"{answer}\",\"annotations\":[]}}]}}"
            );
            let body = format!(
                "event: response.created\ndata: {{\"type\":\"response.created\",\"sequence_number\":0,\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"muse-spark-1.2\"}}}}\n\nevent: response.completed\ndata: {{\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"model\":\"muse-spark-1.2\",\"status\":\"completed\",\"output\":[{output}],\"usage\":{{\"input_tokens\":20,\"input_tokens_details\":{{\"cached_tokens\":0}},\"output_tokens\":5,\"output_tokens_details\":{{\"reasoning_tokens\":0}},\"total_tokens\":25}}}}}}\n\ndata: [DONE]\n\n"
            );
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(headers.as_bytes())
                .await
                .expect("headers should write");
            stream
                .write_all(body.as_bytes())
                .await
                .expect("response should write");
            stream.shutdown().await.expect("response should close");
        }
        requests_sender
            .send(captured)
            .unwrap_or_else(|_| panic!("requests should be observed"));
    });
    (format!("http://{address}"), requests_receiver, server)
}

async fn spawn_image_provider() -> (
    String,
    oneshot::Receiver<String>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("image provider fixture should bind");
    let address = listener
        .local_addr()
        .expect("image provider fixture should have an address");
    let (captured_sender, captured_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("request should connect");
        let request = read_http_request(&mut stream).await;
        captured_sender
            .send(String::from_utf8(request).expect("request should be UTF-8"))
            .unwrap_or_else(|_| panic!("request should be observed"));
        let output = "{\"id\":\"msg_image\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"phase\":\"final_answer\",\"content\":[{\"type\":\"output_text\",\"text\":\"I see the image.\",\"annotations\":[]}]}";
        let body = format!(
            "event: response.created\ndata: {{\"type\":\"response.created\",\"sequence_number\":0,\"response\":{{\"id\":\"resp_image\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"gpt-5.4\"}}}}\n\nevent: response.completed\ndata: {{\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{{\"id\":\"resp_image\",\"object\":\"response\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[{output}],\"usage\":{{\"input_tokens\":20,\"input_tokens_details\":{{\"cached_tokens\":0}},\"output_tokens\":5,\"output_tokens_details\":{{\"reasoning_tokens\":0}},\"total_tokens\":25}}}}}}\n\ndata: [DONE]\n\n"
        );
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream
            .write_all(headers.as_bytes())
            .await
            .expect("image headers should write");
        stream
            .write_all(body.as_bytes())
            .await
            .expect("image response should write");
        stream
            .shutdown()
            .await
            .expect("image response should close");
    });
    (format!("http://{address}"), captured_receiver, server)
}

async fn spawn_successful_provider() -> (
    String,
    oneshot::Receiver<String>,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("provider fixture should bind");
    let address = listener
        .local_addr()
        .expect("provider fixture should have an address");
    let (captured_sender, captured_receiver) = oneshot::channel();
    let (complete_sender, complete_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("request should connect");
        let request = read_http_request(&mut stream).await;
        let captured = String::from_utf8(request).expect("request should be UTF-8");
        captured_sender
            .send(captured)
            .unwrap_or_else(|_| panic!("request should be observed"));
        let first = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"sequence_number\":0,\"response\":{\"id\":\"resp_run_1\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"muse-spark-1.2\"}}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"sequence_number\":1,\"output_index\":0,\"content_index\":0,\"item_id\":\"msg_run_1\",\"delta\":\"durable answer\",\"logprobs\":[]}\n\n"
        );
        let terminal = concat!(
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"sequence_number\":2,\"response\":{\"id\":\"resp_run_1\",\"object\":\"response\",\"model\":\"muse-spark-1.2\",\"status\":\"completed\",\"output\":[{\"id\":\"msg_run_1\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"durable answer\",\"annotations\":[]}]}],\"usage\":{\"input_tokens\":8,\"input_tokens_details\":{\"cached_tokens\":0},\"output_tokens\":3,\"output_tokens_details\":{\"reasoning_tokens\":0},\"total_tokens\":11}}}\n\n",
            "data: [DONE]\n\n"
        );
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            first.len() + terminal.len()
        );
        stream
            .write_all(headers.as_bytes())
            .await
            .expect("response headers should be written");
        stream
            .write_all(first.as_bytes())
            .await
            .expect("streaming response should begin");
        complete_receiver
            .await
            .unwrap_or_else(|_| panic!("provider completion should be released"));
        stream
            .write_all(terminal.as_bytes())
            .await
            .expect("terminal response should be written");
        stream.shutdown().await.expect("response should close");
    });
    (
        format!("http://{address}"),
        captured_receiver,
        complete_sender,
        server,
    )
}

async fn spawn_direct_tool_loop_provider() -> (
    String,
    oneshot::Receiver<Vec<String>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("tool provider fixture should bind");
    let address = listener
        .local_addr()
        .expect("tool provider should have an address");
    let (requests_sender, requests_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let read_arguments = r#"{"path":"note.txt","offset":1,"limit":20}"#;
        let edit_arguments =
            r#"{"path":"note.txt","replacements":[{"old_text":"before","new_text":"after"}]}"#;
        let bash_arguments = r#"{"command":"printf shell > shell.txt; printf 'shell stdout'"}"#;
        let outputs = [
            format!(
                "{{\"id\":\"fc_read\",\"type\":\"function_call\",\"status\":\"completed\",\"call_id\":\"provider_read\",\"name\":\"read\",\"arguments\":{}}}",
                serde_json::to_string(read_arguments).expect("arguments should encode")
            ),
            format!(
                "{{\"id\":\"fc_edit\",\"type\":\"function_call\",\"status\":\"completed\",\"call_id\":\"provider_edit\",\"name\":\"edit\",\"arguments\":{}}}",
                serde_json::to_string(edit_arguments).expect("arguments should encode")
            ),
            format!(
                "{{\"id\":\"fc_bash\",\"type\":\"function_call\",\"status\":\"completed\",\"call_id\":\"provider_bash\",\"name\":\"bash\",\"arguments\":{}}}",
                serde_json::to_string(bash_arguments).expect("arguments should encode")
            ),
            "{\"id\":\"msg_final\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"phase\":\"final_answer\",\"content\":[{\"type\":\"output_text\",\"text\":\"Updated note.txt.\",\"annotations\":[]}] }".to_owned(),
        ];
        let mut captured = Vec::new();
        for (index, output) in outputs.into_iter().enumerate() {
            let (mut stream, _) = listener
                .accept()
                .await
                .expect("tool request should connect");
            captured.push(
                String::from_utf8(read_http_request(&mut stream).await)
                    .expect("tool request should be UTF-8"),
            );
            let response_id = format!("resp_tool_{}", index + 1);
            let body = format!(
                "event: response.created\ndata: {{\"type\":\"response.created\",\"sequence_number\":0,\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"muse-spark-1.2\"}}}}\n\nevent: response.completed\ndata: {{\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"model\":\"muse-spark-1.2\",\"status\":\"completed\",\"output\":[{output}],\"usage\":{{\"input_tokens\":8,\"input_tokens_details\":{{\"cached_tokens\":0}},\"output_tokens\":3,\"output_tokens_details\":{{\"reasoning_tokens\":0}},\"total_tokens\":11}}}}}}\n\ndata: [DONE]\n\n"
            );
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(headers.as_bytes())
                .await
                .expect("tool response headers should write");
            stream
                .write_all(body.as_bytes())
                .await
                .expect("tool response should write");
            stream.shutdown().await.expect("tool response should close");
        }
        requests_sender
            .send(captured)
            .unwrap_or_else(|_| panic!("tool requests should be observed"));
    });
    (format!("http://{address}"), requests_receiver, server)
}

async fn spawn_read_image_tool_provider() -> (
    String,
    oneshot::Receiver<Vec<String>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("read image provider should bind");
    let address = listener
        .local_addr()
        .expect("provider should have an address");
    let (requests_sender, requests_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let arguments = r#"{"path":"picture.png","offset":1,"limit":1}"#;
        let outputs = [
            format!(
                "{{\"id\":\"fc_read_image\",\"type\":\"function_call\",\"status\":\"completed\",\"call_id\":\"provider_read_image\",\"name\":\"read\",\"arguments\":{}}}",
                serde_json::to_string(arguments).expect("arguments should encode")
            ),
            "{\"id\":\"msg_image_final\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"phase\":\"final_answer\",\"content\":[{\"type\":\"output_text\",\"text\":\"I inspected picture.png.\",\"annotations\":[]}]}".to_owned(),
        ];
        let mut captured = Vec::new();
        for (index, output) in outputs.into_iter().enumerate() {
            let (mut stream, _) = listener.accept().await.expect("provider should connect");
            captured.push(
                String::from_utf8(read_http_request(&mut stream).await)
                    .expect("provider request should be UTF-8"),
            );
            let response_id = format!("resp_read_image_{}", index + 1);
            let body = format!(
                "event: response.created\ndata: {{\"type\":\"response.created\",\"sequence_number\":0,\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"gpt-5.4\"}}}}\n\nevent: response.completed\ndata: {{\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[{output}],\"usage\":{{\"input_tokens\":16,\"input_tokens_details\":{{\"cached_tokens\":0}},\"output_tokens\":4,\"output_tokens_details\":{{\"reasoning_tokens\":0}},\"total_tokens\":20}}}}}}\n\ndata: [DONE]\n\n"
            );
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(headers.as_bytes())
                .await
                .expect("provider headers should write");
            stream
                .write_all(body.as_bytes())
                .await
                .expect("provider response should write");
            stream.shutdown().await.expect("provider should close");
        }
        requests_sender
            .send(captured)
            .unwrap_or_else(|_| panic!("requests should be observed"));
    });
    (format!("http://{address}"), requests_receiver, server)
}

async fn spawn_ipython_tool_loop_provider() -> (
    String,
    oneshot::Receiver<Vec<String>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("IPython provider fixture should bind");
    let address = listener
        .local_addr()
        .expect("IPython provider fixture should have an address");
    let (requests_sender, requests_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let cells = [r#"{"cell":"value = 41"}"#, r#"{"cell":"value + 1"}"#];
        let outputs = [
            format!(
                "{{\"id\":\"fc_python_1\",\"type\":\"function_call\",\"status\":\"completed\",\"call_id\":\"provider_python_1\",\"name\":\"ipython\",\"arguments\":{}}}",
                serde_json::to_string(cells[0]).expect("arguments should encode")
            ),
            format!(
                "{{\"id\":\"fc_python_2\",\"type\":\"function_call\",\"status\":\"completed\",\"call_id\":\"provider_python_2\",\"name\":\"ipython\",\"arguments\":{}}}",
                serde_json::to_string(cells[1]).expect("arguments should encode")
            ),
            "{\"id\":\"msg_final\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"phase\":\"final_answer\",\"content\":[{\"type\":\"output_text\",\"text\":\"Persistent Python returned 42.\",\"annotations\":[]}]}".to_owned(),
        ];
        let mut captured = Vec::new();
        for (index, output) in outputs.into_iter().enumerate() {
            let (mut stream, _) = listener.accept().await.expect("provider should connect");
            captured.push(
                String::from_utf8(read_http_request(&mut stream).await)
                    .expect("provider request should be UTF-8"),
            );
            let response_id = format!("resp_ipython_{}", index + 1);
            let body = format!(
                "event: response.created\ndata: {{\"type\":\"response.created\",\"sequence_number\":0,\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"muse-spark-1.2\"}}}}\n\nevent: response.completed\ndata: {{\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"model\":\"muse-spark-1.2\",\"status\":\"completed\",\"output\":[{output}],\"usage\":{{\"input_tokens\":8,\"input_tokens_details\":{{\"cached_tokens\":0}},\"output_tokens\":3,\"output_tokens_details\":{{\"reasoning_tokens\":0}},\"total_tokens\":11}}}}}}\n\ndata: [DONE]\n\n"
            );
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(headers.as_bytes())
                .await
                .expect("provider headers should write");
            stream
                .write_all(body.as_bytes())
                .await
                .expect("provider response should write");
            stream.shutdown().await.expect("provider should close");
        }
        requests_sender
            .send(captured)
            .unwrap_or_else(|_| panic!("provider requests should be observed"));
    });
    (format!("http://{address}"), requests_receiver, server)
}

async fn spawn_web_search_tool_loop_provider() -> (
    String,
    oneshot::Receiver<Vec<String>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("web tool provider fixture should bind");
    let address = listener
        .local_addr()
        .expect("web tool provider fixture should have an address");
    let (requests_sender, requests_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let search_arguments = r#"{"query":"current Rust release"}"#;
        let outputs = [
            format!(
                "{{\"id\":\"fc_web\",\"type\":\"function_call\",\"status\":\"completed\",\"call_id\":\"provider_web\",\"name\":\"web_search\",\"arguments\":{}}}",
                serde_json::to_string(search_arguments).expect("arguments should encode")
            ),
            "{\"id\":\"msg_final\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"phase\":\"final_answer\",\"content\":[{\"type\":\"output_text\",\"text\":\"Found the Rust site.\",\"annotations\":[]}]}".to_owned(),
        ];
        let mut captured = Vec::new();
        for (index, output) in outputs.into_iter().enumerate() {
            let (mut stream, _) = listener.accept().await.expect("provider should connect");
            captured.push(
                String::from_utf8(read_http_request(&mut stream).await)
                    .expect("provider request should be UTF-8"),
            );
            let response_id = format!("resp_web_{}", index + 1);
            let body = format!(
                "event: response.created\ndata: {{\"type\":\"response.created\",\"sequence_number\":0,\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"muse-spark-1.2\"}}}}\n\nevent: response.completed\ndata: {{\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"model\":\"muse-spark-1.2\",\"status\":\"completed\",\"output\":[{output}],\"usage\":{{\"input_tokens\":8,\"input_tokens_details\":{{\"cached_tokens\":0}},\"output_tokens\":3,\"output_tokens_details\":{{\"reasoning_tokens\":0}},\"total_tokens\":11}}}}}}\n\ndata: [DONE]\n\n"
            );
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(headers.as_bytes())
                .await
                .expect("provider headers should write");
            stream
                .write_all(body.as_bytes())
                .await
                .expect("provider response should write");
            stream.shutdown().await.expect("provider should close");
        }
        requests_sender
            .send(captured)
            .unwrap_or_else(|_| panic!("provider requests should be observed"));
    });
    (format!("http://{address}"), requests_receiver, server)
}

async fn spawn_search_adapter() -> (
    String,
    oneshot::Receiver<String>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("search fixture should bind");
    let address = listener
        .local_addr()
        .expect("search fixture should have an address");
    let (request_sender, request_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("search should connect");
        let request = read_http_request(&mut stream).await;
        request_sender
            .send(String::from_utf8(request).expect("search request should be UTF-8"))
            .unwrap_or_else(|_| panic!("search request should be observed"));
        let body = r#"{"web":{"results":[{"title":"Rust","url":"https://www.rust-lang.org/","description":"Rust is a programming language"}]}}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("search response should write");
        stream.shutdown().await.expect("search should close");
    });
    (format!("http://{address}/search"), request_receiver, server)
}

async fn spawn_stalled_provider() -> (String, oneshot::Receiver<()>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("provider fixture should bind");
    let address = listener
        .local_addr()
        .expect("provider fixture should have an address");
    let (dispatched_sender, dispatched_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("request should connect");
        read_http_request(&mut stream).await;
        let body = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"sequence_number\":0,\"response\":{\"id\":\"resp_stalled\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"muse-spark-1.2\"}}\n\n"
        );
        let headers = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n";
        stream
            .write_all(headers.as_bytes())
            .await
            .expect("headers should write");
        stream
            .write_all(format!("{:x}\r\n{body}\r\n", body.len()).as_bytes())
            .await
            .expect("stream chunk should write");
        dispatched_sender
            .send(())
            .unwrap_or_else(|_| panic!("dispatch should be observed"));
        let mut byte = [0_u8; 1];
        match time::timeout(Duration::from_secs(5), stream.read(&mut byte)).await {
            Ok(Ok(0)) => {}
            Ok(Err(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::BrokenPipe
                ) => {}
            Ok(Ok(_)) => panic!("cancelled request should not send more bytes"),
            Ok(Err(error)) => panic!("cancelled request closed unexpectedly: {error}"),
            Err(_) => panic!("cancelled request should close promptly"),
        }
    });
    (format!("http://{address}"), dispatched_receiver, server)
}

async fn read_http_request(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut received = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 4096];
        let bytes = stream.read(&mut chunk).await.expect("request should read");
        assert_ne!(bytes, 0, "request ended before headers");
        received.extend_from_slice(&chunk[..bytes]);
        assert!(received.len() <= 5 * 1024 * 1024);
        if let Some(position) = received.windows(4).position(|value| value == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let headers = std::str::from_utf8(&received[..header_end]).expect("headers should be UTF-8");
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
        })
        .unwrap_or(0);
    while received.len() - header_end < content_length {
        let mut chunk = [0_u8; 4096];
        let bytes = stream.read(&mut chunk).await.expect("body should read");
        assert_ne!(bytes, 0, "request ended before body");
        received.extend_from_slice(&chunk[..bytes]);
    }
    received
}

fn write_test_skill(root: &std::path::Path, name: &str, description: &str, body: &str) {
    let directory = root.join(".agents/skills").join(name);
    fs::create_dir_all(&directory).expect("skill directory should be created");
    fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n"),
    )
    .expect("skill should be written");
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

struct TestRoot(PathBuf);

impl TestRoot {
    fn new(label: &str) -> Self {
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).expect("test randomness should be available");
        let encoded = nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path = std::env::temp_dir().join(format!(
            "morons-supervisor-{label}-{}-{encoded}",
            process::id()
        ));
        fs::create_dir(&path).expect("test root should be created");
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("test root should be owner-only");
        #[cfg(windows)]
        fence_windows::harden_private_directory(&path)
            .expect("Windows test root should be hardened");
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
