use morons_protocol::{
    ApplicationError, ApplicationEvent, ApplicationRequest, ApplicationResponse, ClientMessage,
    MutationRequestId, ServerMessage, SessionCatalogEventCursor, SessionId, SessionSummary,
    read_client_message, write_server_message,
};

use super::{SessionClient, SessionClientError};

#[tokio::test(flavor = "current_thread")]
async fn session_client_correlates_create_get_and_list_requests() {
    let (client_connection, mut server) = tokio::io::duplex(4096);
    let mut client = SessionClient::from_negotiated_connection(client_connection);
    let mutation_request_id = MutationRequestId::from_bytes([0x11; 16]);
    let session = SessionSummary {
        id: SessionId::from_bytes([0x22; 16]),
        display_name: Some("Client session".to_owned()),
        created_at_milliseconds: 42,
    };
    let catalog_cursor = SessionCatalogEventCursor::from_bytes(9_u64.to_be_bytes());

    let client_exchange = async {
        let created = client
            .create_session(mutation_request_id, session.display_name.clone())
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
async fn session_subscription_tracks_durable_catalog_cursor() {
    let (client_connection, mut server) = tokio::io::duplex(2048);
    let client = SessionClient::from_negotiated_connection(client_connection);
    let initial_cursor = SessionCatalogEventCursor::from_bytes(9_u64.to_be_bytes());
    let next_cursor = SessionCatalogEventCursor::from_bytes(10_u64.to_be_bytes());
    let session = SessionSummary {
        id: SessionId::from_bytes([0x24; 16]),
        display_name: Some("Subscribed session".to_owned()),
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
            SessionClientError::EventCursorNotMonotonic
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
async fn missing_session_is_returned_as_none() {
    let (client_connection, mut server) = tokio::io::duplex(1024);
    let mut client = SessionClient::from_negotiated_connection(client_connection);
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
    let mut client = SessionClient::from_negotiated_connection(client_connection);

    let client_exchange = async {
        let error = client
            .list_sessions(None, 10)
            .await
            .expect_err("mismatched response should fail");
        assert!(matches!(
            error,
            SessionClientError::ResponseIdentifierMismatch {
                expected_request_id: 1,
                received_request_id: 2,
            }
        ));
        assert!(matches!(
            client
                .list_sessions(None, 10)
                .await
                .expect_err("protocol failure should poison the connection"),
            SessionClientError::ConnectionUnusable
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
