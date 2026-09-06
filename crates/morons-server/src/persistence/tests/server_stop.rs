use super::*;

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
            attachments: Vec::new(),
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
