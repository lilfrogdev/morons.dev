use morons_protocol::{
    ApplicationError, ApplicationEvent, ApplicationRequest, ApplicationResponse, ClientMessage,
    MessageId, MutationRequestId, OpenCodeModelCapabilities, OpenCodeModelRetention,
    OpenCodeModelSummary, OpenCodeModelTrainingUse, OpenCodeService, RunId, RunState, RunSummary,
    ServerMessage, SessionCatalogEventCursor, SessionEventCursor, SessionId, SessionSummary,
    SkillSource, SkillSummary, WorkspaceState, WorkspaceSummary, read_client_message,
    write_server_message,
};

use super::{ApplicationClient, ApplicationClientError};

fn fixture_working_directory() -> String {
    if cfg!(windows) {
        r"C:\projects\example".to_owned()
    } else {
        "/projects/example".to_owned()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn session_client_correlates_create_get_and_list_requests() {
    let (client_connection, mut server) = tokio::io::duplex(4096);
    let mut client = ApplicationClient::from_negotiated_connection(client_connection);
    let mutation_request_id = MutationRequestId::from_bytes([0x11; 16]);
    let session = SessionSummary {
        id: SessionId::from_bytes([0x22; 16]),
        display_name: Some("Client session".to_owned()),
        working_directory: Some(fixture_working_directory()),
        archived: false,
        created_at_milliseconds: 42,
    };
    let catalog_cursor = SessionCatalogEventCursor::from_bytes(9_u64.to_be_bytes());

    let client_exchange = async {
        let created = client
            .create_session_at(
                mutation_request_id,
                session.display_name.clone(),
                fixture_working_directory(),
            )
            .await
            .expect("client should create a session");
        assert_eq!(created, session);

        let found = client
            .get_session(session.id)
            .await
            .expect("client should get a session");
        assert_eq!(found, Some(session.clone()));

        let page = client
            .list_sessions(None, 10)
            .await
            .expect("client should list sessions");
        assert_eq!(page.sessions, vec![session.clone()]);
        assert_eq!(page.next_cursor, None);
        assert_eq!(page.catalog_cursor, catalog_cursor);
    };
    let server_exchange = async {
        let create = read_request(&mut server, 1).await;
        assert_eq!(
            create,
            ApplicationRequest::CreateSession {
                mutation_request_id,
                display_name: Some("Client session".to_owned()),
                working_directory: fixture_working_directory(),
            }
        );
        write_server_message(
            &mut server,
            &ServerMessage::response(
                1,
                ApplicationResponse::SessionCreated {
                    session: session.clone(),
                },
            ),
        )
        .await
        .expect("create response should be written");

        assert_eq!(
            read_request(&mut server, 2).await,
            ApplicationRequest::GetSession {
                session_id: session.id,
            }
        );
        write_server_message(
            &mut server,
            &ServerMessage::response(
                2,
                ApplicationResponse::SessionFound {
                    session: session.clone(),
                },
            ),
        )
        .await
        .expect("get response should be written");

        assert_eq!(
            read_request(&mut server, 3).await,
            ApplicationRequest::ListSessions {
                cursor: None,
                limit: 10,
            }
        );
        write_server_message(
            &mut server,
            &ServerMessage::response(
                3,
                ApplicationResponse::SessionsListed {
                    sessions: vec![session.clone()],
                    next_cursor: None,
                    catalog_cursor,
                },
            ),
        )
        .await
        .expect("list response should be written");
    };

    tokio::join!(client_exchange, server_exchange);
}

#[tokio::test(flavor = "current_thread")]
async fn client_lists_models_with_exact_service_scope() {
    let (client_connection, mut server) = tokio::io::duplex(4096);
    let mut client = ApplicationClient::from_negotiated_connection(client_connection);
    let model = fixture_model_summary(OpenCodeService::Go);

    let expected_model = model.clone();
    let client_exchange = async {
        assert_eq!(
            client
                .list_open_code_models(OpenCodeService::Go)
                .await
                .expect("model query should succeed"),
            vec![expected_model]
        );
    };
    let server_exchange = async {
        assert_eq!(
            read_request(&mut server, 1).await,
            ApplicationRequest::ListOpenCodeModels {
                service: OpenCodeService::Go,
            }
        );
        write_server_message(
            &mut server,
            &ServerMessage::response(
                1,
                ApplicationResponse::OpenCodeModelsListed {
                    service: OpenCodeService::Go,
                    models: vec![model],
                },
            ),
        )
        .await
        .expect("model response should be written");
    };

    tokio::join!(client_exchange, server_exchange);
}

#[tokio::test(flavor = "current_thread")]
async fn client_lists_bounded_session_scoped_skills() {
    let (client_connection, mut server) = tokio::io::duplex(4096);
    let mut client = ApplicationClient::from_negotiated_connection(client_connection);
    let session_id = SessionId::from_bytes([0x27; 16]);
    let skill = SkillSummary {
        name: "skill-creator".to_owned(),
        description: "Creates standards-compatible Agent Skills.".to_owned(),
        source: SkillSource::Bundled,
    };
    let expected = skill.clone();

    let client_exchange = async {
        let catalog = client
            .list_session_skills(session_id)
            .await
            .expect("skill query should succeed");
        assert_eq!(catalog.skills, vec![expected]);
        assert_eq!(catalog.warnings, vec!["one invalid project skill"]);
    };
    let server_exchange = async {
        assert_eq!(
            read_request(&mut server, 1).await,
            ApplicationRequest::ListSessionSkills { session_id }
        );
        write_server_message(
            &mut server,
            &ServerMessage::response(
                1,
                ApplicationResponse::SessionSkillsListed {
                    session_id,
                    skills: vec![skill],
                    warnings: vec!["one invalid project skill".to_owned()],
                },
            ),
        )
        .await
        .expect("skill response should be written");
    };

    tokio::join!(client_exchange, server_exchange);
}

#[tokio::test(flavor = "current_thread")]
async fn client_rejects_cross_service_model_metadata() {
    let (client_connection, mut server) = tokio::io::duplex(4096);
    let mut client = ApplicationClient::from_negotiated_connection(client_connection);
    let mut model = fixture_model_summary(OpenCodeService::Go);
    model.service = OpenCodeService::Zen;

    let client_exchange = async {
        assert!(matches!(
            client.list_open_code_models(OpenCodeService::Go).await,
            Err(ApplicationClientError::EventScopeMismatch)
        ));
        assert!(matches!(
            client.list_sessions(None, 10).await,
            Err(ApplicationClientError::ConnectionUnusable)
        ));
    };
    let server_exchange = async {
        read_request(&mut server, 1).await;
        write_server_message(
            &mut server,
            &ServerMessage::response(
                1,
                ApplicationResponse::OpenCodeModelsListed {
                    service: OpenCodeService::Go,
                    models: vec![model],
                },
            ),
        )
        .await
        .expect("model response should be written");
    };

    tokio::join!(client_exchange, server_exchange);
}

#[tokio::test(flavor = "current_thread")]
async fn client_submits_inspects_and_cancels_exact_run() {
    let (client_connection, mut server) = tokio::io::duplex(4096);
    let mut client = ApplicationClient::from_negotiated_connection(client_connection);
    let mutation_request_id = MutationRequestId::from_bytes([0x31; 16]);
    let cancellation_request_id = MutationRequestId::from_bytes([0x32; 16]);
    let session_id = SessionId::from_bytes([0x33; 16]);
    let run_id = RunId::from_bytes([0x34; 16]);
    let user_message_id = MessageId::from_bytes([0x35; 16]);
    let run = RunSummary {
        id: run_id,
        session_id,
        user_message_id,
        service: OpenCodeService::Zen,
        model_id: "muse-spark-1.2".to_owned(),
        protocol_revision: 1,
        credential_generation: 2,
        context_policy_version: 1,
        tool_catalog_version: 0,
        tool_limits_version: 0,
        state: RunState::Active,
        cancellation_requested: false,
        failure: None,
        accepted_at_milliseconds: 41,
        updated_at_milliseconds: 42,
    };

    let client_exchange = async {
        let accepted = client
            .submit_session_input(
                mutation_request_id,
                session_id,
                "hello".to_owned(),
                OpenCodeService::Zen,
                "muse-spark-1.2".to_owned(),
            )
            .await
            .expect("input should be accepted");
        assert_eq!(accepted.user_message_id, user_message_id);
        assert_eq!(accepted.run, run);
        assert_eq!(
            client
                .get_run(session_id, run_id)
                .await
                .expect("run query should succeed"),
            Some(run.clone())
        );
        let cancelled = client
            .cancel_run(cancellation_request_id, session_id, run_id)
            .await
            .expect("cancellation should resolve");
        assert_eq!(cancelled.run_id, run_id);
        assert_eq!(cancelled.state, RunState::Active);
        assert!(cancelled.cancellation_requested);
    };
    let server_exchange = async {
        assert_eq!(
            read_request(&mut server, 1).await,
            ApplicationRequest::SubmitSessionInput {
                mutation_request_id,
                session_id,
                text: "hello".to_owned(),
                attachments: Vec::new(),
                service: OpenCodeService::Zen,
                model_id: "muse-spark-1.2".to_owned(),
            }
        );
        write_server_message(
            &mut server,
            &ServerMessage::response(
                1,
                ApplicationResponse::SessionInputAccepted {
                    user_message_id,
                    run: run.clone(),
                },
            ),
        )
        .await
        .expect("acceptance should write");
        assert_eq!(
            read_request(&mut server, 2).await,
            ApplicationRequest::GetRun { session_id, run_id }
        );
        write_server_message(
            &mut server,
            &ServerMessage::response(2, ApplicationResponse::RunFound { run: run.clone() }),
        )
        .await
        .expect("run response should write");
        assert_eq!(
            read_request(&mut server, 3).await,
            ApplicationRequest::CancelRun {
                mutation_request_id: cancellation_request_id,
                session_id,
                run_id,
            }
        );
        write_server_message(
            &mut server,
            &ServerMessage::response(
                3,
                ApplicationResponse::RunCancellationResolved {
                    run_id,
                    state: RunState::Active,
                    cancellation_requested: true,
                },
            ),
        )
        .await
        .expect("cancellation response should write");
    };

    tokio::join!(client_exchange, server_exchange);
}

#[tokio::test(flavor = "current_thread")]
async fn client_acknowledges_only_the_exact_uncertain_run() {
    let (client_connection, mut server) = tokio::io::duplex(4096);
    let mut client = ApplicationClient::from_negotiated_connection(client_connection);
    let mutation_request_id = MutationRequestId::from_bytes([0x69; 16]);
    let session_id = SessionId::from_bytes([0x6a; 16]);
    let run_id = RunId::from_bytes([0x6b; 16]);
    let workspace = WorkspaceSummary {
        state: WorkspaceState::Ready,
        file_count: 2,
        logical_bytes: 20,
        block_reason: None,
        blocked_run_id: None,
        blocked_tool: None,
    };
    let client_exchange = async {
        assert_eq!(
            client
                .acknowledge_tool_uncertainty(mutation_request_id, session_id, run_id)
                .await
                .expect("acknowledgement should succeed"),
            workspace
        );
    };
    let server_exchange = async {
        assert_eq!(
            read_request(&mut server, 1).await,
            ApplicationRequest::AcknowledgeToolUncertainty {
                mutation_request_id,
                session_id,
                run_id,
            }
        );
        write_server_message(
            &mut server,
            &ServerMessage::response(
                1,
                ApplicationResponse::ToolUncertaintyAcknowledged {
                    session_id,
                    run_id,
                    workspace,
                },
            ),
        )
        .await
        .expect("acknowledgement response should write");
    };
    tokio::join!(client_exchange, server_exchange);
}

#[tokio::test(flavor = "current_thread")]
async fn client_stops_the_server_with_one_stable_mutation() {
    let (client_connection, mut server) = tokio::io::duplex(2048);
    let mut client = ApplicationClient::from_negotiated_connection(client_connection);
    let mutation_request_id = MutationRequestId::from_bytes([0x41; 16]);

    let client_exchange = async {
        let accepted = client
            .stop_server(mutation_request_id)
            .await
            .expect("server stop should be accepted");
        assert!(accepted.current_server_stopping);
        assert!(matches!(
            client.get_session(SessionId::from_bytes([0x42; 16])).await,
            Err(ApplicationClientError::ConnectionUnusable)
        ));
    };
    let server_exchange = async {
        assert_eq!(
            read_request(&mut server, 1).await,
            ApplicationRequest::StopServer {
                mutation_request_id,
            }
        );
        write_server_message(
            &mut server,
            &ServerMessage::response(
                1,
                ApplicationResponse::ServerStopAccepted {
                    current_server_stopping: true,
                },
            ),
        )
        .await
        .expect("server stop response should write");
    };

    tokio::join!(client_exchange, server_exchange);
}

#[tokio::test(flavor = "current_thread")]
async fn session_subscription_tracks_durable_catalog_cursor() {
    let (client_connection, mut server) = tokio::io::duplex(2048);
    let client = ApplicationClient::from_negotiated_connection(client_connection);
    let initial_cursor = SessionCatalogEventCursor::from_bytes(9_u64.to_be_bytes());
    let next_cursor = SessionCatalogEventCursor::from_bytes(10_u64.to_be_bytes());
    let session = SessionSummary {
        id: SessionId::from_bytes([0x24; 16]),
        display_name: Some("Subscribed session".to_owned()),
        working_directory: Some(fixture_working_directory()),
        archived: false,
        created_at_milliseconds: 43,
    };

    let client_exchange = async {
        let mut subscription = client
            .subscribe_to_session_catalog(initial_cursor)
            .await
            .expect("client should start a subscription");
        assert_eq!(subscription.cursor(), initial_cursor);
        let event = subscription
            .next_event()
            .await
            .expect("client should receive a durable event");
        assert_eq!(
            event,
            ApplicationEvent::SessionCreated {
                cursor: next_cursor,
                session: session.clone(),
            }
        );
        assert_eq!(subscription.cursor(), next_cursor);
        assert!(matches!(
            subscription
                .next_event()
                .await
                .expect_err("duplicate catalog cursor should fail"),
            ApplicationClientError::EventCursorNotMonotonic
        ));
    };
    let server_exchange = async {
        assert_eq!(
            read_request(&mut server, 1).await,
            ApplicationRequest::SubscribeSessionCatalog {
                cursor: initial_cursor,
            }
        );
        write_server_message(
            &mut server,
            &ServerMessage::response(
                1,
                ApplicationResponse::SessionCatalogSubscriptionStarted {
                    cursor: initial_cursor,
                },
            ),
        )
        .await
        .expect("subscription response should be written");
        write_server_message(
            &mut server,
            &ServerMessage::event(ApplicationEvent::SessionCreated {
                cursor: next_cursor,
                session: session.clone(),
            }),
        )
        .await
        .expect("session event should be written");
        write_server_message(
            &mut server,
            &ServerMessage::event(ApplicationEvent::SessionCreated {
                cursor: next_cursor,
                session: session.clone(),
            }),
        )
        .await
        .expect("duplicate session event should be written");
    };

    tokio::join!(client_exchange, server_exchange);
}

#[tokio::test(flavor = "current_thread")]
async fn session_subscription_tracks_durable_and_ephemeral_run_events() {
    let (client_connection, mut server) = tokio::io::duplex(4096);
    let client = ApplicationClient::from_negotiated_connection(client_connection);
    let session_id = SessionId::from_bytes([0x41; 16]);
    let run_id = RunId::from_bytes([0x42; 16]);
    let initial_cursor = session_event_cursor(session_id, 9);
    let workspace_cursor = session_event_cursor(session_id, 10);
    let active_cursor = session_event_cursor(session_id, 11);
    let terminal_cursor = session_event_cursor(session_id, 12);
    let mut run = RunSummary {
        id: run_id,
        session_id,
        user_message_id: MessageId::from_bytes([0x43; 16]),
        service: OpenCodeService::Zen,
        model_id: "muse-spark-1.2".to_owned(),
        protocol_revision: 1,
        credential_generation: 2,
        context_policy_version: 1,
        tool_catalog_version: 0,
        tool_limits_version: 0,
        state: RunState::Active,
        cancellation_requested: false,
        failure: None,
        accepted_at_milliseconds: 41,
        updated_at_milliseconds: 42,
    };

    let client_run = run.clone();
    let client_exchange = async move {
        let mut subscription = client
            .subscribe_to_session(session_id, initial_cursor)
            .await
            .expect("client should start a session subscription");
        assert_eq!(
            subscription
                .next_event()
                .await
                .expect("workspace event should arrive"),
            ApplicationEvent::SessionWorkspaceChanged {
                cursor: workspace_cursor,
                session_id,
                workspace: WorkspaceSummary {
                    state: WorkspaceState::Importing,
                    file_count: 0,
                    logical_bytes: 0,
                    block_reason: None,
                    blocked_run_id: None,
                    blocked_tool: None,
                },
            }
        );
        assert_eq!(
            subscription
                .next_event()
                .await
                .expect("active run event should arrive"),
            ApplicationEvent::SessionRunChanged {
                cursor: active_cursor,
                run: client_run,
            }
        );
        assert!(matches!(
            subscription
                .next_event()
                .await
                .expect("first delta should arrive"),
            ApplicationEvent::SessionAssistantDelta {
                sequence: 1,
                ref delta,
                ..
            } if delta == "partial"
        ));
        assert!(matches!(
            subscription
                .next_event()
                .await
                .expect("a delta sequence gap should be allowed"),
            ApplicationEvent::SessionAssistantDelta { sequence: 3, .. }
        ));
        assert!(matches!(
            subscription
                .next_event()
                .await
                .expect("terminal run event should arrive"),
            ApplicationEvent::SessionRunChanged {
                cursor,
                run: RunSummary { state: RunState::Succeeded, .. },
            } if cursor == terminal_cursor
        ));
        assert_eq!(subscription.cursor(), terminal_cursor);
        assert!(matches!(
            subscription
                .next_event()
                .await
                .expect_err("a post-terminal delta should fail closed"),
            ApplicationClientError::EventScopeMismatch
        ));
    };
    let server_exchange = async {
        assert_eq!(
            read_request(&mut server, 1).await,
            ApplicationRequest::SubscribeSession {
                session_id,
                cursor: initial_cursor,
            }
        );
        write_server_message(
            &mut server,
            &ServerMessage::response(
                1,
                ApplicationResponse::SessionSubscriptionStarted {
                    session_id,
                    cursor: initial_cursor,
                },
            ),
        )
        .await
        .expect("subscription response should be written");
        write_server_message(
            &mut server,
            &ServerMessage::event(ApplicationEvent::SessionWorkspaceChanged {
                cursor: workspace_cursor,
                session_id,
                workspace: WorkspaceSummary {
                    state: WorkspaceState::Importing,
                    file_count: 0,
                    logical_bytes: 0,
                    block_reason: None,
                    blocked_run_id: None,
                    blocked_tool: None,
                },
            }),
        )
        .await
        .expect("workspace event should be written");
        write_server_message(
            &mut server,
            &ServerMessage::event(ApplicationEvent::SessionRunChanged {
                cursor: active_cursor,
                run: run.clone(),
            }),
        )
        .await
        .expect("active event should be written");
        for (sequence, delta) in [(1, "partial"), (3, " answer")] {
            write_server_message(
                &mut server,
                &ServerMessage::event(ApplicationEvent::SessionAssistantDelta {
                    session_id,
                    run_id,
                    sequence,
                    delta: delta.to_owned(),
                    refusal: false,
                }),
            )
            .await
            .expect("delta should be written");
        }
        run.state = RunState::Succeeded;
        run.updated_at_milliseconds = 43;
        write_server_message(
            &mut server,
            &ServerMessage::event(ApplicationEvent::SessionRunChanged {
                cursor: terminal_cursor,
                run,
            }),
        )
        .await
        .expect("terminal event should be written");
        write_server_message(
            &mut server,
            &ServerMessage::event(ApplicationEvent::SessionAssistantDelta {
                session_id,
                run_id,
                sequence: 4,
                delta: "stale".to_owned(),
                refusal: false,
            }),
        )
        .await
        .expect("stale delta should be written");
    };

    tokio::join!(client_exchange, server_exchange);
}

#[tokio::test(flavor = "current_thread")]
async fn missing_session_is_returned_as_none() {
    let (client_connection, mut server) = tokio::io::duplex(1024);
    let mut client = ApplicationClient::from_negotiated_connection(client_connection);
    let session_id = SessionId::from_bytes([0x33; 16]);

    let client_exchange = async {
        assert_eq!(
            client
                .get_session(session_id)
                .await
                .expect("not found should be a valid query result"),
            None
        );
    };
    let server_exchange = async {
        read_request(&mut server, 1).await;
        write_server_message(
            &mut server,
            &ServerMessage::request_failed(1, ApplicationError::SessionNotFound),
        )
        .await
        .expect("not-found response should be written");
    };

    tokio::join!(client_exchange, server_exchange);
}

#[tokio::test(flavor = "current_thread")]
async fn mismatched_response_identifier_is_rejected() {
    let (client_connection, mut server) = tokio::io::duplex(1024);
    let mut client = ApplicationClient::from_negotiated_connection(client_connection);

    let client_exchange = async {
        let error = client
            .list_sessions(None, 10)
            .await
            .expect_err("mismatched response should fail");
        assert!(matches!(
            error,
            ApplicationClientError::ResponseIdentifierMismatch {
                expected_request_id: 1,
                received_request_id: 2,
            }
        ));
        assert!(matches!(
            client
                .list_sessions(None, 10)
                .await
                .expect_err("protocol failure should poison the connection"),
            ApplicationClientError::ConnectionUnusable
        ));
    };
    let server_exchange = async {
        read_request(&mut server, 1).await;
        write_server_message(
            &mut server,
            &ServerMessage::response(
                2,
                ApplicationResponse::SessionsListed {
                    sessions: Vec::new(),
                    next_cursor: None,
                    catalog_cursor: SessionCatalogEventCursor::beginning(),
                },
            ),
        )
        .await
        .expect("mismatched response should be written");
    };

    tokio::join!(client_exchange, server_exchange);
}

fn fixture_model_summary(service: OpenCodeService) -> OpenCodeModelSummary {
    OpenCodeModelSummary {
        service,
        id: "grok-4.6".to_owned(),
        display_name: "Grok 4.6".to_owned(),
        available: true,
        responses_protocol_revision: 1,
        capabilities: OpenCodeModelCapabilities {
            text_input: true,
            image_input: false,
            text_output: true,
            reasoning: true,
            reasoning_continuation: false,
            tool_calls: true,
        },
        maximum_input_tokens: 96_000,
        maximum_output_tokens: 32_000,
        training_use: OpenCodeModelTrainingUse::NotUsed,
        retention: OpenCodeModelRetention::UpToThirtyDays,
    }
}

fn session_event_cursor(session_id: SessionId, sequence: u64) -> SessionEventCursor {
    let mut bytes = [0_u8; 24];
    bytes[..16].copy_from_slice(session_id.as_bytes());
    bytes[16..].copy_from_slice(&sequence.to_be_bytes());
    SessionEventCursor::from_bytes(bytes)
}

async fn read_request<S>(connection: &mut S, expected_request_id: u64) -> ApplicationRequest
where
    S: tokio::io::AsyncRead + Unpin,
{
    let message = read_client_message(connection)
        .await
        .expect("client request should be readable")
        .expect("client should send a request");
    let ClientMessage::Request {
        request_id,
        request,
    } = message
    else {
        panic!("client sent an unexpected message");
    };
    assert_eq!(request_id, expected_request_id);
    request
}
