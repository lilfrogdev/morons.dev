use super::*;

#[tokio::test(flavor = "current_thread")]
async fn session_commands_cross_the_application_and_transport_boundaries() {
    let root = TestRoot::new("application-boundary");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let application = ServerApplication::from_session_store(store);
    let mutation_request_id = ProtocolMutationRequestId::from_bytes([0x17; 16]);
    let (client_connection, mut server_connection) = tokio::io::duplex(16 * 1024);

    let client_exchange = async {
        let mut client = ApplicationClient::from_negotiated_connection(client_connection);
        let session = client
            .create_session(mutation_request_id, Some("Application session".to_owned()))
            .await
            .expect("client should create a session");
        assert_eq!(session.display_name.as_deref(), Some("Application session"));
        assert_eq!(
            session.working_directory.as_deref(),
            std::env::current_dir()
                .expect("client working directory should resolve")
                .to_str()
        );

        let retry = client
            .create_session(mutation_request_id, Some("Application session".to_owned()))
            .await
            .expect("client retry should return the same session");
        assert_eq!(retry, session);

        let conflict = client
            .create_session(mutation_request_id, Some("Changed".to_owned()))
            .await
            .expect_err("conflicting request should fail");
        assert!(matches!(
            conflict,
            ApplicationClientError::Application(ApplicationError::RequestConflict)
        ));

        assert_eq!(
            client
                .get_session(session.id)
                .await
                .expect("client should get a session"),
            Some(session.clone())
        );
        assert_eq!(
            client
                .get_session(ProtocolSessionId::from_bytes([0x18; 16]))
                .await
                .expect("missing session should be a valid result"),
            None
        );

        let invalid_cursor = client
            .list_sessions(Some(ProtocolSessionListCursor::from_bytes([0xff; 16])), 10)
            .await
            .expect_err("unsupported cursor should fail");
        assert!(matches!(
            invalid_cursor,
            ApplicationClientError::Application(ApplicationError::InvalidRequest)
        ));

        let page = client
            .list_sessions(None, 10)
            .await
            .expect("client should list sessions");
        assert_eq!(page.sessions, vec![session.clone()]);
        assert_eq!(page.next_cursor, None);
        let archived = client
            .set_session_archived(
                ProtocolMutationRequestId::from_bytes([0x19; 16]),
                session.id,
                true,
            )
            .await
            .expect("client should archive the session");
        assert!(archived.archived);
        client
            .delete_session(
                ProtocolMutationRequestId::from_bytes([0x1a; 16]),
                session.id,
            )
            .await
            .expect("client should delete the archived session");
        assert!(
            client
                .get_session(session.id)
                .await
                .expect("deleted session query should succeed")
                .is_none()
        );
    };
    let server_exchange = async {
        handle_local_owner_requests(&mut server_connection, &application)
            .await
            .expect("server should handle session requests");
    };

    tokio::join!(client_exchange, server_exchange);
}

#[tokio::test(flavor = "current_thread")]
async fn session_subscription_replays_commits_after_a_gap_free_snapshot() {
    let root = TestRoot::new("session-subscription");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let application = ServerApplication::from_session_store(store);
    let (command_connection, mut command_server) = tokio::io::duplex(16 * 1024);
    let (subscription_connection, mut subscription_server) = tokio::io::duplex(16 * 1024);

    let client_exchange = async {
        let mut commands = ApplicationClient::from_negotiated_connection(command_connection);
        commands
            .create_session(
                ProtocolMutationRequestId::from_bytes([0x31; 16]),
                Some("Snapshot session".to_owned()),
            )
            .await
            .expect("initial session should be created");
        let snapshot = commands
            .list_sessions(None, 100)
            .await
            .expect("session snapshot should be listed");

        let created_after_snapshot = commands
            .create_session(
                ProtocolMutationRequestId::from_bytes([0x32; 16]),
                Some("Event session".to_owned()),
            )
            .await
            .expect("session after snapshot should be created");
        let mut subscription =
            ApplicationClient::from_negotiated_connection(subscription_connection)
                .subscribe_to_session_catalog(snapshot.catalog_cursor)
                .await
                .expect("session event subscription should start");
        let event = subscription
            .next_event()
            .await
            .expect("committed event should be delivered");
        assert_eq!(
            event,
            ApplicationEvent::SessionCreated {
                cursor: subscription.cursor(),
                session: created_after_snapshot,
            }
        );
        assert!(subscription.cursor().as_bytes() > snapshot.catalog_cursor.as_bytes());

        let created_while_subscribed = commands
            .create_session(
                ProtocolMutationRequestId::from_bytes([0x33; 16]),
                Some("Live event session".to_owned()),
            )
            .await
            .expect("session during subscription should be created");
        let live_event = subscription
            .next_event()
            .await
            .expect("live committed event should be delivered");
        assert_eq!(
            live_event,
            ApplicationEvent::SessionCreated {
                cursor: subscription.cursor(),
                session: created_while_subscribed.clone(),
            }
        );
        let archived = commands
            .set_session_archived(
                ProtocolMutationRequestId::from_bytes([0x34; 16]),
                created_while_subscribed.id,
                true,
            )
            .await
            .expect("subscribed session should archive");
        assert_eq!(
            subscription
                .next_event()
                .await
                .expect("archive event should be delivered"),
            ApplicationEvent::SessionChanged {
                cursor: subscription.cursor(),
                session: archived,
            }
        );
        commands
            .delete_session(
                ProtocolMutationRequestId::from_bytes([0x35; 16]),
                created_while_subscribed.id,
            )
            .await
            .expect("subscribed session should delete");
        assert_eq!(
            subscription
                .next_event()
                .await
                .expect("removal event should be delivered"),
            ApplicationEvent::SessionRemoved {
                cursor: subscription.cursor(),
                session_id: created_while_subscribed.id,
            }
        );
        drop(subscription);
        drop(commands);
    };
    let command_server_exchange = async {
        handle_local_owner_requests(&mut command_server, &application)
            .await
            .expect("server should handle session commands");
    };
    let subscription_server_exchange = async {
        handle_local_owner_requests(&mut subscription_server, &application)
            .await
            .expect("server should handle session subscription");
    };

    tokio::time::timeout(SESSION_SUBSCRIPTION_TEST_TIMEOUT, async {
        tokio::join!(
            client_exchange,
            command_server_exchange,
            subscription_server_exchange
        );
    })
    .await
    .expect("session subscription exchange should not time out");
}

#[tokio::test(flavor = "current_thread")]
async fn slow_session_catalog_subscriber_is_disconnected() {
    let root = TestRoot::new("slow-subscriber");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    store
        .create_session(
            MutationRequestId::from_bytes([0x41; 16]),
            Some("Buffered event".to_owned()),
        )
        .await
        .expect("session should be created");
    let application = ServerApplication::from_session_store(store);
    let (client_connection, mut server_connection) = tokio::io::duplex(64);

    let client_exchange = async {
        let subscription = ApplicationClient::from_negotiated_connection(client_connection)
            .subscribe_to_session_catalog(ProtocolSessionCatalogEventCursor::beginning())
            .await
            .expect("subscription should start");
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(subscription);
    };
    let server_exchange = async {
        handle_local_owner_requests(&mut server_connection, &application)
            .await
            .expect_err("slow subscriber should be disconnected")
    };
    let ((), error) = tokio::join!(client_exchange, server_exchange);
    assert!(matches!(error, ConnectionError::SubscriptionWriteTimedOut));
}

#[tokio::test(flavor = "current_thread")]
async fn slow_session_event_subscriber_is_disconnected() {
    let root = TestRoot::new("slow-session-event-subscriber");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    store
        .set_open_code_credential(
            MutationRequestId::from_bytes([0x42; 16]),
            0,
            b"not-a-real-slow-subscriber-key".to_vec(),
        )
        .await
        .expect("credential should be configured");
    let session = store
        .create_session(MutationRequestId::from_bytes([0x43; 16]), None)
        .await
        .expect("session should be created");
    store
        .accept_session_input(
            MutationRequestId::from_bytes([0x44; 16]),
            session.id,
            "x".repeat(64 * 1024),
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
        .expect("large session input should be accepted");
    let application = ServerApplication::from_session_store(store);
    let (client_connection, mut server_connection) = tokio::io::duplex(64);
    let protocol_session_id = ProtocolSessionId::from_bytes(*session.id.as_bytes());
    let cursor = protocol_session_event_cursor(protocol_session_id, session.created_sequence);

    let client_exchange = async {
        let subscription = ApplicationClient::from_negotiated_connection(client_connection)
            .subscribe_to_session(protocol_session_id, cursor)
            .await
            .expect("subscription should start");
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(subscription);
    };
    let server_exchange = async {
        handle_local_owner_requests(&mut server_connection, &application)
            .await
            .expect_err("slow session subscriber should be disconnected")
    };
    let ((), error) = tokio::join!(client_exchange, server_exchange);
    assert!(matches!(error, ConnectionError::SubscriptionWriteTimedOut));
}
