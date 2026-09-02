use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use morons_cli::{ApplicationClient, ApplicationClientError};
use morons_protocol::{
    ApplicationError, ApplicationEvent, ApplicationRequest,
    MutationRequestId as ProtocolMutationRequestId, OpenCodeService,
    SessionCatalogEventCursor as ProtocolSessionCatalogEventCursor,
    SessionEventCursor as ProtocolSessionEventCursor, SessionId as ProtocolSessionId,
    SessionListCursor as ProtocolSessionListCursor,
};
use rusqlite::{Connection, config::DbConfig, params};

use crate::{
    ConnectionError,
    application::{ApplicationOutcome, ServerApplication},
    handle_local_owner_requests,
};

use super::{
    MutationRequestId, PersistenceError, RunModelSelection, RunOpenCodeService,
    SessionCatalogEventCursor, SessionId, SessionListCursor, SessionStore, database,
    paths::StoragePaths, types::create_session_fingerprint,
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

const SESSION_SUBSCRIPTION_TEST_TIMEOUT: Duration = Duration::from_secs(15);
static TEST_PATH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[tokio::test(flavor = "current_thread")]
async fn server_stop_is_idempotent_generation_bound_and_audited() {
    let root = TestRoot::new("server-stop");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let request_id = MutationRequestId::from_bytes([0x09; 16]);
    let first_epoch = [0x31; 16];
    let successor_epoch = [0x32; 16];

    let accepted = store
        .request_server_stop(request_id, first_epoch)
        .await
        .expect("server stop should be accepted");
    assert!(accepted.signal_current_supervisor);
    assert_eq!(accepted.accepted_host_epoch, first_epoch);
    let retry = store
        .request_server_stop(request_id, successor_epoch)
        .await
        .expect("server stop retry should return its prior result");
    assert!(!retry.signal_current_supervisor);
    assert_eq!(retry.accepted_host_epoch, first_epoch);
    let second_request_id = MutationRequestId::from_bytes([0x0d; 16]);
    let second = store
        .request_server_stop(second_request_id, first_epoch)
        .await
        .expect("second stop should be durably accepted");
    assert!(!second.signal_current_supervisor);
    assert_eq!(second.accepted_host_epoch, first_epoch);
    assert!(matches!(
        store.create_session(request_id, None).await,
        Err(PersistenceError::RequestConflict)
    ));
    drop(store);

    let reopened = SessionStore::open_at(root.path()).expect("session store should reopen");
    let recovered = reopened
        .request_server_stop(request_id, successor_epoch)
        .await
        .expect("recovered stop retry should return its prior result");
    assert!(!recovered.signal_current_supervisor);
    assert_eq!(recovered.accepted_host_epoch, first_epoch);
    let successor_request_id = MutationRequestId::from_bytes([0x0e; 16]);
    let successor = reopened
        .request_server_stop(successor_request_id, successor_epoch)
        .await
        .expect("successor stop should be accepted for its generation");
    assert!(successor.signal_current_supervisor);
    assert_eq!(successor.accepted_host_epoch, successor_epoch);
    drop(reopened);

    let connection = Connection::open(root.path().join("data").join("sessions.sqlite3"))
        .expect("server stop database should open");
    let (requests, audits, operation): (i64, i64, i64) = connection
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM server_stop_requests),
                (SELECT COUNT(*) FROM server_audit_facts),
                (SELECT operation_kind FROM mutation_requests WHERE request_id = ?1)",
            [&request_id.as_bytes()[..]],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("server stop facts should be readable");
    assert_eq!((requests, audits, operation), (3, 3, 6));
}

#[tokio::test(flavor = "current_thread")]
async fn corrupted_server_stop_facts_fail_closed() {
    let root = TestRoot::new("server-stop-corruption");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let request_id = MutationRequestId::from_bytes([0x0f; 16]);
    store
        .request_server_stop(request_id, [0x41; 16])
        .await
        .expect("server stop should be accepted");
    drop(store);

    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection = Connection::open(&database_path).expect("database should open for corruption");
    connection
        .execute(
            "UPDATE server_stop_requests SET operation_fingerprint = ?2 WHERE request_id = ?1",
            params![&request_id.as_bytes()[..], &[0_u8; 32][..]],
        )
        .expect("server stop fingerprint should be corrupted");
    drop(connection);
    let error = session_store_open_error(&root, "corrupted server stop should fail closed");
    assert!(matches!(error, PersistenceError::InvalidState { .. }));
}

#[tokio::test(flavor = "current_thread")]
async fn accepted_server_stop_signals_once_and_rejects_new_run_input() {
    let root = TestRoot::new("server-stop-application");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let session = store
        .create_session(MutationRequestId::from_bytes([0x0a; 16]), None)
        .await
        .expect("session should be created");
    let application = ServerApplication::from_session_store(store);
    let mut shutdown = application.subscribe_shutdown_requests();
    let stop_id = ProtocolMutationRequestId::from_bytes([0x0b; 16]);
    assert!(matches!(
        application
            .execute_for_local_owner(ApplicationRequest::StopServer {
                mutation_request_id: stop_id,
            })
            .await
            .expect("server stop should be accepted"),
        ApplicationOutcome::StopServerAccepted {
            current_server_stopping: true
        }
    ));
    shutdown
        .changed()
        .await
        .expect("first server stop should signal shutdown");
    assert!(*shutdown.borrow());

    let input = application
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: ProtocolMutationRequestId::from_bytes([0x0c; 16]),
            session_id: ProtocolSessionId::from_bytes(*session.id.as_bytes()),
            text: "must not start after shutdown acceptance".to_owned(),
            service: OpenCodeService::Zen,
            model_id: "muse-spark-1.2".to_owned(),
        })
        .await;
    assert!(matches!(input, Err(ApplicationError::ServiceUnavailable)));
    drop(application);

    let reopened = SessionStore::open_at(root.path()).expect("session store should reopen");
    let successor = ServerApplication::from_session_store(reopened);
    let mut successor_shutdown = successor.subscribe_shutdown_requests();
    assert!(matches!(
        successor
            .execute_for_local_owner(ApplicationRequest::StopServer {
                mutation_request_id: stop_id,
            })
            .await
            .expect("stop retry should return its committed result"),
        ApplicationOutcome::StopServerAccepted {
            current_server_stopping: false
        }
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(25), successor_shutdown.changed())
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn sessions_are_idempotent_queryable_paginated_and_durable() {
    let root = TestRoot::new("session-roundtrip");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let first = store
        .create_session(
            MutationRequestId::from_bytes([0x11; 16]),
            Some("First session".to_owned()),
        )
        .await
        .expect("first session should be created");
    let retry = store
        .create_session(
            MutationRequestId::from_bytes([0x11; 16]),
            Some("First session".to_owned()),
        )
        .await
        .expect("an exact retry should return the original session");
    assert_eq!(retry, first);

    let second = store
        .create_session(
            MutationRequestId::from_bytes([0x22; 16]),
            Some("Second session".to_owned()),
        )
        .await
        .expect("second session should be created");
    let third = store
        .create_session(MutationRequestId::from_bytes([0x33; 16]), None)
        .await
        .expect("third session should be created");

    assert_eq!(
        store
            .get_session(first.id)
            .await
            .expect("session lookup should succeed"),
        Some(first.clone())
    );
    assert_eq!(
        store
            .get_session(SessionId::from_bytes([0xff; 16]))
            .await
            .expect("missing session lookup should succeed"),
        None
    );

    let first_page = store
        .list_sessions(None, 2)
        .await
        .expect("first page should be listed");
    assert_eq!(first_page.sessions, vec![first.clone(), second.clone()]);
    let cursor = first_page
        .next_cursor
        .expect("first page should have a continuation cursor");
    let snapshot_catalog_cursor = first_page.catalog_cursor;
    let fourth = store
        .create_session(
            MutationRequestId::from_bytes([0x34; 16]),
            Some("Fourth session".to_owned()),
        )
        .await
        .expect("fourth session should be created");
    let second_page = store
        .list_sessions(Some(cursor), 2)
        .await
        .expect("second page should be listed");
    assert_eq!(second_page.sessions, vec![third.clone()]);
    assert_eq!(second_page.next_cursor, None);
    assert_eq!(second_page.catalog_cursor, snapshot_catalog_cursor);

    let replay = store
        .read_session_catalog_events(snapshot_catalog_cursor, 100)
        .await
        .expect("events after the snapshot should replay");
    assert_eq!(replay.events.len(), 1);
    assert_eq!(replay.events[0].session, fourth.clone());
    assert_eq!(replay.events[0].cursor, replay.high_water);

    let complete_replay = store
        .read_session_catalog_events(SessionCatalogEventCursor::from_sequence(0), 100)
        .await
        .expect("complete event history should replay");
    assert_eq!(complete_replay.events.len(), 4);
    assert_eq!(
        complete_replay
            .events
            .iter()
            .map(|event| event.session.id)
            .collect::<Vec<_>>(),
        vec![first.id, second.id, third.id, fourth.id]
    );

    let first_workspace_id = first.workspace_id;
    drop(store);

    let reopened = SessionStore::open_at(root.path()).expect("session store should reopen");
    let persisted = reopened
        .get_session(first.id)
        .await
        .expect("persisted session should be readable")
        .expect("persisted session should exist");
    assert_eq!(persisted, first);
    assert_eq!(persisted.workspace_id, first_workspace_id);
    let replay_after_restart = reopened
        .read_session_catalog_events(snapshot_catalog_cursor, 100)
        .await
        .expect("durable events should replay after restart");
    assert_eq!(replay_after_restart.events.len(), 1);
    assert_eq!(replay_after_restart.events[0].session, fourth);
}

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
        assert_eq!(page.sessions, vec![session]);
        assert_eq!(page.next_cursor, None);
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
                session: created_while_subscribed,
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

#[tokio::test(flavor = "current_thread")]
async fn conflicting_request_identifiers_fail_without_creating_another_session() {
    let root = TestRoot::new("request-conflict");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let request_id = MutationRequestId::from_bytes([0x44; 16]);
    store
        .create_session(request_id, Some("Original".to_owned()))
        .await
        .expect("original request should succeed");

    let error = store
        .create_session(request_id, Some("Changed".to_owned()))
        .await
        .expect_err("conflicting retry should fail");
    assert!(matches!(error, PersistenceError::RequestConflict));
    assert_eq!(
        store
            .list_sessions(None, 100)
            .await
            .expect("sessions should be listed")
            .sessions
            .len(),
        1
    );
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_requests_fail_before_reaching_the_worker() {
    let root = TestRoot::new("invalid-input");
    let store = SessionStore::open_at(root.path()).expect("session store should open");

    let zero_identifier = store
        .create_session(MutationRequestId::from_bytes([0; 16]), None)
        .await
        .expect_err("zero request identifier should fail");
    assert!(matches!(
        zero_identifier,
        PersistenceError::InvalidInput { .. }
    ));

    let zero_credential_identifier = store
        .set_open_code_credential(
            MutationRequestId::from_bytes([0; 16]),
            0,
            b"not-a-real-key".to_vec(),
        )
        .await
        .expect_err("zero credential request identifier should fail");
    assert!(matches!(
        zero_credential_identifier,
        PersistenceError::InvalidInput { .. }
    ));

    let invalid_api_key = store
        .set_open_code_credential(
            MutationRequestId::from_bytes([3; 16]),
            0,
            b"invalid key".to_vec(),
        )
        .await
        .expect_err("invalid API key should fail");
    assert!(matches!(
        invalid_api_key,
        PersistenceError::InvalidInput { .. }
    ));

    let control_character = store
        .create_session(
            MutationRequestId::from_bytes([1; 16]),
            Some("invalid\nname".to_owned()),
        )
        .await
        .expect_err("control character should fail");
    assert!(matches!(
        control_character,
        PersistenceError::InvalidInput { .. }
    ));

    let oversized = store
        .create_session(
            MutationRequestId::from_bytes([2; 16]),
            Some("x".repeat(257)),
        )
        .await
        .expect_err("oversized name should fail");
    assert!(matches!(oversized, PersistenceError::InvalidInput { .. }));

    let empty_page = store
        .list_sessions(None, 0)
        .await
        .expect_err("zero page size should fail");
    assert!(matches!(empty_page, PersistenceError::InvalidInput { .. }));

    let oversized_cursor = store
        .list_sessions(Some(SessionListCursor::new(u64::MAX, 0)), 1)
        .await
        .expect_err("oversized cursor should fail");
    assert!(matches!(
        oversized_cursor,
        PersistenceError::InvalidInput { .. }
    ));

    let empty_event_page = store
        .read_session_catalog_events(SessionCatalogEventCursor::from_sequence(0), 0)
        .await
        .expect_err("zero event page size should fail");
    assert!(matches!(
        empty_event_page,
        PersistenceError::InvalidInput { .. }
    ));

    let future_catalog_cursor = store
        .read_session_catalog_events(SessionCatalogEventCursor::from_sequence(1), 1)
        .await
        .expect_err("future event cursor should fail");
    assert!(matches!(
        future_catalog_cursor,
        PersistenceError::InvalidInput { .. }
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn damaged_rebuildable_projections_are_restored_from_canonical_facts() {
    let root = TestRoot::new("projection-repair");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let session = store
        .create_session(MutationRequestId::from_bytes([0x55; 16]), None)
        .await
        .expect("session should be created");
    drop(store);

    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection =
        Connection::open(&database_path).expect("database should open for test damage");
    connection
        .execute("DELETE FROM sessions", [])
        .expect("session projection should be deleted");
    connection
        .execute("DELETE FROM delivery_events", [])
        .expect("event projection should be deleted");
    drop(connection);

    let reopened = SessionStore::open_at(root.path()).expect("projection repair should succeed");
    assert_eq!(
        reopened
            .get_session(session.id)
            .await
            .expect("repaired session should be queried"),
        Some(session)
    );
    drop(reopened);

    let connection = Connection::open(database_path).expect("repaired database should open");
    let delivery_events: i64 = connection
        .query_row("SELECT COUNT(*) FROM delivery_events", [], |row| row.get(0))
        .expect("event projection count should be readable");
    assert_eq!(delivery_events, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn startup_reconciles_a_dispatched_workspace_before_finalizing_the_session() {
    let root = TestRoot::new("workspace-recovery");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    drop(store);

    let request_id = [0x81; 16];
    let session_id = [0x82; 16];
    let workspace_id = [0x83; 16];
    let fingerprint = create_session_fingerprint(None);
    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection =
        Connection::open(&database_path).expect("database should open for crash setup");
    connection
        .execute(
            "INSERT INTO mutation_requests (
                request_id,
                operation_kind,
                accepted_sequence,
                accepted_at_milliseconds
            ) VALUES (?1, 1, 1, 1000)",
            [&request_id[..]],
        )
        .expect("mutation registry record should be inserted");
    connection
        .execute(
            "INSERT INTO session_creation_requests (
                request_id,
                operation_fingerprint,
                session_id,
                workspace_id,
                display_name,
                accepted_sequence,
                accepted_at_milliseconds,
                state
            ) VALUES (?1, ?2, ?3, ?4, NULL, 1, 1000, 1)",
            params![
                &request_id[..],
                &fingerprint[..],
                &session_id[..],
                &workspace_id[..]
            ],
        )
        .expect("prepared request should be inserted");
    connection
        .execute(
            "INSERT INTO audit_facts (
                audit_id,
                audit_sequence,
                request_id,
                session_id,
                audit_kind,
                created_at_milliseconds
            ) VALUES (?1, 2, ?2, ?3, 1, 1000)",
            params![&[0x85_u8; 16][..], &request_id[..], &session_id[..]],
        )
        .expect("accepted audit fact should be inserted");
    connection
        .execute(
            "INSERT INTO workspace_operation_facts (
                fact_id,
                fact_sequence,
                request_id,
                workspace_id,
                operation_kind,
                created_at_milliseconds
            ) VALUES (?1, 3, ?2, ?3, 1, 1001)",
            params![&[0x86_u8; 16][..], &request_id[..], &workspace_id[..]],
        )
        .expect("dispatch fact should be inserted");
    connection
        .execute(
            "INSERT INTO audit_facts (
                audit_id,
                audit_sequence,
                request_id,
                session_id,
                audit_kind,
                created_at_milliseconds
            ) VALUES (?1, 4, ?2, ?3, 2, 1001)",
            params![&[0x87_u8; 16][..], &request_id[..], &session_id[..]],
        )
        .expect("dispatch audit fact should be inserted");
    connection
        .execute(
            "UPDATE logical_sequences SET next_value = 5 WHERE singleton = 1",
            [],
        )
        .expect("logical sequence should advance past crash facts");
    drop(connection);

    let paths = StoragePaths::prepare(root.path()).expect("storage paths should remain valid");
    paths
        .provision_workspace(&workspace_id)
        .expect("workspace effect should complete before simulated crash");

    let recovered = SessionStore::open_at(root.path()).expect("startup recovery should succeed");
    let session = recovered
        .get_session(SessionId::from_bytes(session_id))
        .await
        .expect("recovered session should be queried")
        .expect("recovered session should exist");
    assert_eq!(session.workspace_id, workspace_id);
    drop(recovered);

    let connection = Connection::open(database_path).expect("recovered database should open");
    let state: i64 = connection
        .query_row(
            "SELECT state FROM session_creation_requests WHERE request_id = ?1",
            [&request_id[..]],
            |row| row.get(0),
        )
        .expect("request state should be readable");
    let workspace_facts: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM workspace_operation_facts WHERE request_id = ?1",
            [&request_id[..]],
            |row| row.get(0),
        )
        .expect("workspace fact count should be readable");
    assert_eq!(state, 2);
    assert_eq!(workspace_facts, 2);
}

#[test]
fn schema_version_one_migrates_to_version_nine() {
    let root = TestRoot::new("schema-v1-migration");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let (initialization_path, file) = paths
        .create_database_initialization_file(&[0xe1; 16])
        .expect("initialization file should be created");
    drop(file);
    let connection =
        Connection::open(&initialization_path).expect("version one migration fixture should open");
    connection
        .execute_batch(include_str!("schema_v1.sql"))
        .expect("version one schema should initialize");
    let request_id = [0xe5_u8; 16];
    let session_id = [0xe6_u8; 16];
    let workspace_id = [0xe7_u8; 16];
    let fingerprint = create_session_fingerprint(None);
    connection
        .execute(
            "INSERT INTO session_creation_requests (
                request_id,
                operation_fingerprint,
                session_id,
                workspace_id,
                display_name,
                accepted_sequence,
                accepted_at_milliseconds,
                state
             ) VALUES (?1, ?2, ?3, ?4, NULL, 1, 1000, 0)",
            params![
                &request_id[..],
                &fingerprint[..],
                &session_id[..],
                &workspace_id[..]
            ],
        )
        .expect("version one request fixture should be inserted");
    connection
        .execute(
            "INSERT INTO audit_facts (
                audit_id,
                audit_sequence,
                request_id,
                session_id,
                audit_kind,
                created_at_milliseconds
             ) VALUES (?1, 2, ?2, ?3, 1, 1000)",
            params![&[0xe8_u8; 16][..], &request_id[..], &session_id[..]],
        )
        .expect("version one audit fixture should be inserted");
    connection
        .execute(
            "UPDATE logical_sequences SET next_value = 3 WHERE singleton = 1",
            [],
        )
        .expect("version one sequence should advance past fixtures");
    drop(connection);
    paths
        .install_database(&initialization_path)
        .expect("version one database should install");

    let connection = database::open(&paths).expect("version one database should migrate");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 9);
    let mutation_operation: i64 = connection
        .query_row(
            "SELECT operation_kind FROM mutation_requests WHERE request_id = ?1",
            [&request_id[..]],
            |row| row.get(0),
        )
        .expect("migrated request should be registered");
    assert_eq!(mutation_operation, 1);
}

#[test]
fn stale_private_migration_backup_temporary_file_is_removed() {
    let root = TestRoot::new("stale-migration-backup");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let (temporary_path, file) = paths
        .create_migration_backup_file(2, &[0xc1; 16])
        .expect("temporary migration backup should be created");
    drop(file);
    drop(paths);

    StoragePaths::prepare(root.path()).expect("stale backup cleanup should succeed");
    assert!(!temporary_path.exists());
}

#[test]
fn schema_version_two_migrates_to_version_nine() {
    let root = TestRoot::new("schema-v2-migration");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let (initialization_path, file) = paths
        .create_database_initialization_file(&[0xd1; 16])
        .expect("initialization file should be created");
    drop(file);
    let connection =
        Connection::open(&initialization_path).expect("version two fixture should open");
    connection
        .execute_batch(include_str!("schema_v1.sql"))
        .expect("version one schema should initialize");
    connection
        .execute_batch(include_str!("schema_v2.sql"))
        .expect("version two schema should initialize");
    let request_id = [0xd2_u8; 16];
    connection
        .execute(
            "INSERT INTO mutation_requests (
                request_id, operation_kind, accepted_sequence, accepted_at_milliseconds
             ) VALUES (?1, 3, 1, 1000)",
            [&request_id[..]],
        )
        .expect("version two mutation should be inserted");
    connection
        .execute(
            "INSERT INTO credential_mutation_requests (
                request_id, operation_kind, expected_generation,
                accepted_sequence, accepted_at_milliseconds, state,
                result_generation, result_configured
             ) VALUES (?1, 3, 0, 1, 1000, 3, NULL, NULL)",
            [&request_id[..]],
        )
        .expect("version two credential request should be inserted");
    connection
        .execute(
            "UPDATE logical_sequences SET next_value = 2 WHERE singleton = 1",
            [],
        )
        .expect("version two sequence should advance past fixture");
    drop(connection);
    paths
        .install_database(&initialization_path)
        .expect("version two database should install");

    let connection = database::open(&paths).expect("version two database should migrate");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 9);
    let operation: i64 = connection
        .query_row(
            "SELECT operation_kind FROM mutation_requests WHERE request_id = ?1",
            [&request_id[..]],
            |row| row.get(0),
        )
        .expect("version two mutation should survive migration");
    assert_eq!(operation, 3);
    let backup_path = root
        .path()
        .join("backups")
        .join("sessions-before-schema-v2.sqlite3");
    let backup = Connection::open(&backup_path).expect("migration backup should open");
    assert_eq!(pragma_integer(&backup, "PRAGMA user_version"), 2);
    let backup_operation: i64 = backup
        .query_row(
            "SELECT operation_kind FROM mutation_requests WHERE request_id = ?1",
            [&request_id[..]],
            |row| row.get(0),
        )
        .expect("migration backup should preserve version two state");
    assert_eq!(backup_operation, 3);
    #[cfg(unix)]
    assert_mode(&backup_path, 0o600);
}

#[test]
fn schema_version_three_migrates_to_version_nine() {
    let root = TestRoot::new("schema-v3-migration");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let (initialization_path, file) = paths
        .create_database_initialization_file(&[0xc3; 16])
        .expect("initialization file should be created");
    drop(file);
    let connection =
        Connection::open(&initialization_path).expect("version three fixture should open");
    connection
        .execute_batch(include_str!("schema_v1.sql"))
        .expect("version one schema should initialize");
    connection
        .execute_batch(include_str!("schema_v2.sql"))
        .expect("version two schema should initialize");
    connection
        .execute_batch(include_str!("schema_v3.sql"))
        .expect("version three schema should initialize");
    drop(connection);
    paths
        .install_database(&initialization_path)
        .expect("version three database should install");

    let connection = database::open(&paths).expect("version three database should migrate");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 9);
    let stop_table: String = connection
        .query_row(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND name = 'server_stop_requests'",
            [],
            |row| row.get(0),
        )
        .expect("server stop table should exist");
    assert_eq!(stop_table, "server_stop_requests");
    let backup_path = root
        .path()
        .join("backups")
        .join("sessions-before-schema-v3.sqlite3");
    let backup = Connection::open(backup_path).expect("version three backup should open");
    assert_eq!(pragma_integer(&backup, "PRAGMA user_version"), 3);
}

#[test]
fn schema_version_four_migrates_to_version_nine() {
    let root = TestRoot::new("schema-v4-migration");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let (initialization_path, file) = paths
        .create_database_initialization_file(&[0xc4; 16])
        .expect("initialization file should be created");
    drop(file);
    let connection =
        Connection::open(&initialization_path).expect("version four fixture should open");
    connection
        .execute_batch(include_str!("schema_v1.sql"))
        .expect("version one schema should initialize");
    connection
        .execute_batch(include_str!("schema_v2.sql"))
        .expect("version two schema should initialize");
    connection
        .execute_batch(include_str!("schema_v3.sql"))
        .expect("version three schema should initialize");
    connection
        .execute_batch(include_str!("schema_v4.sql"))
        .expect("version four schema should initialize");
    drop(connection);
    paths
        .install_database(&initialization_path)
        .expect("version four database should install");

    let connection = database::open(&paths).expect("version four database should migrate");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 9);
    let import_table: String = connection
        .query_row(
            "SELECT name FROM sqlite_schema
             WHERE type = 'table' AND name = 'repository_import_requests'",
            [],
            |row| row.get(0),
        )
        .expect("repository import table should exist");
    assert_eq!(import_table, "repository_import_requests");
    let backup_path = root
        .path()
        .join("backups")
        .join("sessions-before-schema-v4.sqlite3");
    let backup = Connection::open(backup_path).expect("version four backup should open");
    assert_eq!(pragma_integer(&backup, "PRAGMA user_version"), 4);
}

#[test]
fn schema_version_five_migrates_to_version_nine() {
    let root = TestRoot::new("schema-v5-migration");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let (initialization_path, file) = paths
        .create_database_initialization_file(&[0xc5; 16])
        .expect("initialization file should be created");
    drop(file);
    let connection =
        Connection::open(&initialization_path).expect("version five fixture should open");
    connection
        .execute_batch(include_str!("schema_v1.sql"))
        .expect("version one schema should initialize");
    connection
        .execute_batch(include_str!("schema_v2.sql"))
        .expect("version two schema should initialize");
    connection
        .execute_batch(include_str!("schema_v3.sql"))
        .expect("version three schema should initialize");
    connection
        .execute_batch(include_str!("schema_v4.sql"))
        .expect("version four schema should initialize");
    connection
        .execute_batch(include_str!("schema_v5.sql"))
        .expect("version five schema should initialize");
    drop(connection);
    paths
        .install_database(&initialization_path)
        .expect("version five database should install");

    let connection = database::open(&paths).expect("version five database should migrate");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 9);
    let tool_table: String = connection
        .query_row(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND name = 'tool_calls'",
            [],
            |row| row.get(0),
        )
        .expect("tool call table should exist");
    assert_eq!(tool_table, "tool_calls");
    let backup_path = root
        .path()
        .join("backups")
        .join("sessions-before-schema-v5.sqlite3");
    let backup = Connection::open(backup_path).expect("version five backup should open");
    assert_eq!(pragma_integer(&backup, "PRAGMA user_version"), 5);
}

#[test]
fn schema_version_six_migrates_to_version_nine() {
    let root = TestRoot::new("schema-v6-migration");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let (initialization_path, file) = paths
        .create_database_initialization_file(&[0xc6; 16])
        .expect("version six initialization file should be created");
    drop(file);
    let connection =
        Connection::open(&initialization_path).expect("version six fixture should open");
    for schema in [
        include_str!("schema_v1.sql"),
        include_str!("schema_v2.sql"),
        include_str!("schema_v3.sql"),
        include_str!("schema_v4.sql"),
        include_str!("schema_v5.sql"),
        include_str!("schema_v6.sql"),
    ] {
        connection
            .execute_batch(schema)
            .expect("schema fixture should migrate");
    }
    drop(connection);
    paths
        .install_database(&initialization_path)
        .expect("version six database should install");

    let connection = database::open(&paths).expect("version six database should migrate");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 9);
    let image_table: String = connection
        .query_row(
            "SELECT name FROM sqlite_schema
             WHERE type = 'table' AND name = 'execution_image_requests'",
            [],
            |row| row.get(0),
        )
        .expect("execution image table should exist");
    assert_eq!(image_table, "execution_image_requests");
    let backup = Connection::open(
        root.path()
            .join("backups")
            .join("sessions-before-schema-v6.sqlite3"),
    )
    .expect("version six backup should open");
    assert_eq!(pragma_integer(&backup, "PRAGMA user_version"), 6);
}

#[test]
fn schema_version_seven_migrates_to_version_nine() {
    let root = TestRoot::new("schema-v7-migration");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let (initialization_path, file) = paths
        .create_database_initialization_file(&[0xc7; 16])
        .expect("version seven initialization file should be created");
    drop(file);
    let connection =
        Connection::open(&initialization_path).expect("version seven fixture should open");
    for schema in [
        include_str!("schema_v1.sql"),
        include_str!("schema_v2.sql"),
        include_str!("schema_v3.sql"),
        include_str!("schema_v4.sql"),
        include_str!("schema_v5.sql"),
        include_str!("schema_v6.sql"),
        include_str!("schema_v7.sql"),
    ] {
        connection
            .execute_batch(schema)
            .expect("schema fixture should migrate");
    }
    drop(connection);
    paths
        .install_database(&initialization_path)
        .expect("version seven database should install");
    let connection = database::open(&paths).expect("version seven database should migrate");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 9);
    let generation_table: String = connection
        .query_row(
            "SELECT name FROM sqlite_schema
             WHERE type = 'table' AND name = 'worktree_generation_facts'",
            [],
            |row| row.get(0),
        )
        .expect("generation table should exist");
    assert_eq!(generation_table, "worktree_generation_facts");
    let backup = Connection::open(
        root.path()
            .join("backups")
            .join("sessions-before-schema-v7.sqlite3"),
    )
    .expect("version seven backup should open");
    assert_eq!(pragma_integer(&backup, "PRAGMA user_version"), 7);
}

#[test]
fn schema_version_eight_migrates_to_version_nine() {
    let root = TestRoot::new("schema-v8-migration");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let (initialization_path, file) = paths
        .create_database_initialization_file(&[0xc8; 16])
        .expect("version eight initialization file should be created");
    drop(file);
    let connection =
        Connection::open(&initialization_path).expect("version eight fixture should open");
    for schema in [
        include_str!("schema_v1.sql"),
        include_str!("schema_v2.sql"),
        include_str!("schema_v3.sql"),
        include_str!("schema_v4.sql"),
        include_str!("schema_v5.sql"),
        include_str!("schema_v6.sql"),
        include_str!("schema_v7.sql"),
        include_str!("schema_v8.sql"),
    ] {
        connection
            .execute_batch(schema)
            .expect("schema fixture should migrate");
    }
    drop(connection);
    paths
        .install_database(&initialization_path)
        .expect("version eight database should install");
    let connection = database::open(&paths).expect("version eight database should migrate");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 9);
    let column: String = connection
        .query_row(
            "SELECT name FROM pragma_table_info('run_accepted_facts')
             WHERE name = 'execution_image_generation'",
            [],
            |row| row.get(0),
        )
        .expect("image generation column should exist");
    assert_eq!(column, "execution_image_generation");
    let backup = Connection::open(
        root.path()
            .join("backups/sessions-before-schema-v8.sqlite3"),
    )
    .expect("version eight backup should open");
    assert_eq!(pragma_integer(&backup, "PRAGMA user_version"), 8);
}

#[test]
fn required_sqlite_configuration_is_applied_and_verified() {
    let root = TestRoot::new("sqlite-configuration");
    let paths = StoragePaths::prepare(root.path()).expect("storage paths should be prepared");
    let connection = database::open(&paths).expect("database should open");

    assert_eq!(pragma_integer(&connection, "PRAGMA page_size"), 4096);
    assert_eq!(pragma_integer(&connection, "PRAGMA synchronous"), 3);
    assert_eq!(pragma_integer(&connection, "PRAGMA fullfsync"), 1);
    assert_eq!(pragma_integer(&connection, "PRAGMA foreign_keys"), 1);
    assert_eq!(pragma_integer(&connection, "PRAGMA trusted_schema"), 0);
    assert_eq!(pragma_integer(&connection, "PRAGMA temp_store"), 2);
    assert!(
        connection
            .db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE)
            .expect("defensive mode should be queryable")
    );
    assert!(
        !connection
            .db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_WRITE)
            .expect("attach-write mode should be queryable")
    );
}

#[test]
fn newer_database_schema_fails_closed_without_downgrade() {
    let root = TestRoot::new("newer-schema");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    drop(store);

    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection =
        Connection::open(&database_path).expect("database should open for test change");
    connection
        .execute_batch("PRAGMA user_version = 10;")
        .expect("test schema version should change");
    drop(connection);

    let error = session_store_open_error(&root, "newer schema should fail closed");
    assert!(matches!(error, PersistenceError::InvalidState { .. }));

    let connection = Connection::open(database_path).expect("database should remain readable");
    assert_eq!(pragma_integer(&connection, "PRAGMA user_version"), 10);
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_canonical_request_fingerprint_fails_closed() {
    let root = TestRoot::new("invalid-fingerprint");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let request_id = MutationRequestId::from_bytes([0x91; 16]);
    store
        .create_session(request_id, Some("Canonical input".to_owned()))
        .await
        .expect("session should be created");
    drop(store);

    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection = Connection::open(&database_path).expect("database should open for corruption");
    connection
        .execute(
            "UPDATE session_creation_requests SET operation_fingerprint = ?2 WHERE request_id = ?1",
            params![&request_id.as_bytes()[..], &[0_u8; 32][..]],
        )
        .expect("test fingerprint should be corrupted");
    drop(connection);

    let error = session_store_open_error(&root, "invalid fingerprint should fail closed");
    assert!(matches!(error, PersistenceError::InvalidState { .. }));
}

#[cfg(unix)]
#[test]
fn insecure_existing_data_directory_fails_closed() {
    let root = TestRoot::new("insecure-data-directory");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    drop(store);

    let data_directory = root.path().join("data");
    fs::set_permissions(&data_directory, fs::Permissions::from_mode(0o755))
        .expect("test should weaken data permissions");
    let error = session_store_open_error(&root, "insecure data root should fail closed");
    assert!(matches!(error, PersistenceError::InvalidState { .. }));
}

#[cfg(unix)]
#[test]
fn linked_database_journal_fails_closed() {
    let root = TestRoot::new("linked-journal");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    drop(store);

    let journal_path = root.path().join("data").join("sessions.sqlite3-journal");
    std::os::unix::fs::symlink(root.path().join("missing-journal-target"), journal_path)
        .expect("test journal symlink should be created");

    let error = session_store_open_error(&root, "linked journal should fail closed");
    assert!(matches!(error, PersistenceError::InvalidState { .. }));
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn database_and_workspace_state_are_owner_only() {
    let root = TestRoot::new("owner-only-state");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let session = store
        .create_session(MutationRequestId::from_bytes([0x66; 16]), None)
        .await
        .expect("session should be created");
    store
        .set_open_code_credential(
            MutationRequestId::from_bytes([0x67; 16]),
            0,
            b"not-a-real-permission-key".to_vec(),
        )
        .await
        .expect("credential should be configured");

    assert_mode(&root.path().join("data"), 0o700);
    assert_mode(&root.path().join("workspaces"), 0o700);
    assert_mode(&root.path().join("sandbox-images"), 0o700);
    assert_mode(&root.path().join("sandbox-operations"), 0o700);
    assert_mode(&root.path().join("backups"), 0o700);
    assert_mode(&root.path().join("credentials"), 0o700);
    assert_mode(&root.path().join("data").join("sessions.sqlite3"), 0o600);
    assert_mode(
        &root.path().join("credentials").join("opencode.state"),
        0o600,
    );
    let workspace = root
        .path()
        .join("workspaces")
        .join(encode_hex(&session.workspace_id));
    assert_mode(&workspace, 0o700);
    assert_mode(&workspace.join("identity"), 0o600);
    assert!(
        !root
            .path()
            .join("data")
            .join("sessions.sqlite3-journal")
            .exists()
    );
}

fn session_store_open_error(root: &TestRoot, message: &str) -> PersistenceError {
    match SessionStore::open_at(root.path()) {
        Ok(store) => {
            drop(store);
            panic!("{message}");
        }
        Err(error) => error,
    }
}

fn pragma_integer(connection: &Connection, pragma: &'static str) -> i64 {
    connection
        .query_row(pragma, [], |row| row.get(0))
        .expect("pragma should return an integer")
}

#[cfg(unix)]
fn assert_mode(path: &Path, expected: u32) {
    let metadata = fs::symlink_metadata(path).expect("path metadata should be readable");
    assert_eq!(metadata.mode() & 0o777, expected);
    assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
}

#[cfg(unix)]
fn encode_hex(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn protocol_session_event_cursor(
    session_id: ProtocolSessionId,
    sequence: u64,
) -> ProtocolSessionEventCursor {
    let mut bytes = [0_u8; 24];
    bytes[..16].copy_from_slice(session_id.as_bytes());
    bytes[16..].copy_from_slice(&sequence.to_be_bytes());
    ProtocolSessionEventCursor::from_bytes(bytes)
}

pub(super) struct TestRoot(PathBuf);

impl TestRoot {
    pub(super) fn new(label: &str) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock should be after Unix epoch")
            .as_nanos();
        let sequence = TEST_PATH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "morons-persistence-{label}-{}-{timestamp}-{sequence}",
            std::process::id()
        ));

        #[cfg(unix)]
        {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder
                .create(&path)
                .expect("private test root should be created");
        }
        #[cfg(not(unix))]
        fs::create_dir(&path).expect("test root should be created");
        #[cfg(windows)]
        fence_windows::harden_private_directory(&path)
            .expect("Windows test root should be hardened");

        Self(path)
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Ok(metadata) = fs::symlink_metadata(self.path()) {
            let _ = fs::set_permissions(self.path(), fs::Permissions::from_mode(0o700));
            if metadata.file_type().is_symlink() {
                let _ = fs::remove_file(self.path());
                return;
            }
        }
        let _ = fs::remove_dir_all(self.path());
    }
}
