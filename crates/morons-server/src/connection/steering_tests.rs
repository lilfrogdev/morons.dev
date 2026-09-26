use super::*;
use crate::persistence::{
    MutationRequestId, RunModelSelection, RunService, SessionStore, credential_tests::TestRoot,
};
use morons_protocol::{
    ApplicationRequest as Request, SteeringChange, SteeringMutation, read_server_message,
    write_client_message,
};
use tokio::io::AsyncWriteExt;

async fn fixture(
    root: &TestRoot,
) -> (
    ServerApplication,
    morons_protocol::SessionId,
    morons_protocol::RunId,
) {
    let (store, session_id, run_id) = store_fixture(root).await;
    (
        ServerApplication::from_session_store(store),
        session_id,
        run_id,
    )
}

async fn store_fixture(
    root: &TestRoot,
) -> (
    SessionStore,
    morons_protocol::SessionId,
    morons_protocol::RunId,
) {
    let store = SessionStore::open_for_test(root.path()).unwrap();
    store
        .set_open_code_credential(
            MutationRequestId::from_bytes([1; 16]),
            0,
            b"not-a-real-test-key".to_vec(),
        )
        .await
        .unwrap();
    let session = store
        .create_session_at(
            MutationRequestId::from_bytes([2; 16]),
            None,
            root.path().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    let run = store
        .accept_session_input(
            MutationRequestId::from_bytes([3; 16]),
            session.id,
            "Initial".into(),
            RunModelSelection {
                service: RunService::Zen,
                model_id: "muse-spark-1.2".into(),
                protocol_revision: 1,
                maximum_input_tokens: 96_000,
                maximum_output_tokens: 32_000,
                supports_tool_calls: true,
                supports_image_input: false,
            },
        )
        .await
        .unwrap()
        .run;
    (
        store,
        morons_protocol::SessionId::from_bytes(*session.id.as_bytes()),
        morons_protocol::RunId::from_bytes(*run.id.as_bytes()),
    )
}

async fn request(app: &ServerApplication, request: Request) -> ServerMessage {
    let (mut client, mut server) = tokio::io::duplex(4096);
    let client_work = async {
        write_client_message(&mut client, &ClientMessage::request(1, request))
            .await
            .unwrap();
        let response = read_server_message(&mut client).await.unwrap().unwrap();
        drop(client);
        response
    };
    let (result, response) =
        tokio::join!(handle_local_owner_requests(&mut server, app), client_work);
    result.unwrap();
    response
}

#[tokio::test]
async fn steering_prepares_skills_and_retry_does_not_reload_files() {
    let root = TestRoot::new("steering-prepared-skills");
    let skill_dir = root.path().join(".agents/skills/example");
    std::fs::create_dir_all(&skill_dir).unwrap();
    let skill_file = skill_dir.join("SKILL.md");
    std::fs::write(
        &skill_file,
        "---\nname: example\ndescription: Example skill\n---\nOriginal instructions\n",
    )
    .unwrap();
    let (app, session_id, run_id) = fixture(&root).await;
    let mutation = SteeringMutation {
        request_id: morons_protocol::MutationRequestId::from_bytes([11; 16]),
        session_id,
        expected_revision: 0,
        change: SteeringChange::Enqueue {
            run_id,
            text: "@example".into(),
        },
    };
    let accepted = request(
        &app,
        Request::MutateSteering {
            mutation: mutation.clone(),
        },
    )
    .await;
    let ServerMessage::Response {
        response: ApplicationResponse::SteeringMutated { ref receipt },
        ..
    } = accepted
    else {
        panic!("enqueue receipt")
    };
    let item_id = receipt.item_id.unwrap();
    let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    let snapshot = |id: &[u8; 16]| {
        db.query_row(
            "SELECT skill_context FROM steering_mutation_requests WHERE request_id = ?1",
            [id],
            |row| row.get::<_, String>(0),
        )
        .unwrap()
    };
    let before = snapshot(mutation.request_id.as_bytes());
    assert!(before.contains("Original instructions"));
    std::fs::write(
        &skill_file,
        "---\nname: example\ndescription: Example skill\n---\nChanged instructions\n",
    )
    .unwrap();
    assert_eq!(
        accepted,
        request(
            &app,
            Request::MutateSteering {
                mutation: mutation.clone()
            }
        )
        .await
    );
    assert_eq!(snapshot(mutation.request_id.as_bytes()), before);
    let edit = SteeringMutation {
        request_id: morons_protocol::MutationRequestId::from_bytes([12; 16]),
        expected_revision: 1,
        change: SteeringChange::Edit {
            item_id,
            revision: 1,
            text: "@example revised".into(),
        },
        ..mutation
    };
    assert!(matches!(
        request(
            &app,
            Request::MutateSteering {
                mutation: edit.clone()
            }
        )
        .await,
        ServerMessage::Response {
            response: ApplicationResponse::SteeringMutated { .. },
            ..
        }
    ));
    assert!(snapshot(edit.request_id.as_bytes()).contains("Changed instructions"));
    app.shutdown().await;
}

#[tokio::test]
async fn steering_stop_admission_preserves_retries_and_queue_controls() {
    let root = TestRoot::new("steering-stop-admission");
    let (app, session_id, run_id) = fixture(&root).await;
    let mutation = SteeringMutation {
        request_id: morons_protocol::MutationRequestId::from_bytes([4; 16]),
        session_id,
        expected_revision: 0,
        change: SteeringChange::Enqueue {
            run_id,
            text: "Queued".into(),
        },
    };
    let accepted = request(
        &app,
        Request::MutateSteering {
            mutation: mutation.clone(),
        },
    )
    .await;
    let ServerMessage::Response {
        response: ApplicationResponse::SteeringMutated { ref receipt },
        ..
    } = accepted
    else {
        panic!("enqueue receipt")
    };
    let item_id = receipt.item_id.unwrap();
    assert!(matches!(
        app.execute_for_local_owner(Request::StopServer {
            mutation_request_id: morons_protocol::MutationRequestId::from_bytes([5; 16]),
        })
        .await
        .unwrap(),
        ApplicationOutcome::StopServerAccepted {
            current_server_stopping: true
        }
    ));
    assert_eq!(
        accepted,
        request(
            &app,
            Request::MutateSteering {
                mutation: mutation.clone()
            }
        )
        .await
    );
    let before = request(&app, Request::GetSteering { session_id }).await;
    for (id, change) in [
        (
            6,
            SteeringChange::Enqueue {
                run_id,
                text: "Rejected".into(),
            },
        ),
        (7, SteeringChange::Resume { run_id }),
    ] {
        assert!(matches!(
            request(
                &app,
                Request::MutateSteering {
                    mutation: SteeringMutation {
                        request_id: morons_protocol::MutationRequestId::from_bytes([id; 16]),
                        expected_revision: 1,
                        change,
                        ..mutation.clone()
                    }
                }
            )
            .await,
            ServerMessage::RequestFailed {
                error: morons_protocol::ApplicationError::ServiceUnavailable,
                ..
            }
        ));
    }
    assert_eq!(
        before,
        request(&app, Request::GetSteering { session_id }).await
    );
    let mut conflict = mutation.clone();
    conflict.change = SteeringChange::Pause;
    assert!(matches!(
        request(&app, Request::MutateSteering { mutation: conflict }).await,
        ServerMessage::RequestFailed {
            error: morons_protocol::ApplicationError::RequestConflict,
            ..
        }
    ));
    for (id, revision, change) in [
        (
            8,
            1,
            SteeringChange::Edit {
                item_id,
                revision: 1,
                text: "Edited".into(),
            },
        ),
        (9, 2, SteeringChange::Pause),
        (
            10,
            3,
            SteeringChange::Remove {
                item_id,
                revision: 2,
            },
        ),
    ] {
        assert!(matches!(request(&app, Request::MutateSteering {
            mutation: SteeringMutation {
                request_id: morons_protocol::MutationRequestId::from_bytes([id; 16]),
                expected_revision: revision,
                change,
                ..mutation.clone()
            }
        }).await, ServerMessage::Response {
            response: ApplicationResponse::SteeringMutated { receipt }, ..
        } if receipt.queue_revision == revision + 1));
    }
    app.shutdown().await;
}

#[tokio::test]
async fn steering_connections_retry_unknown_ack_and_replay_without_gaps() {
    let root = TestRoot::new("steering-connections");
    let (app, session_id, run_id) = fixture(&root).await;
    let mutation = SteeringMutation {
        request_id: morons_protocol::MutationRequestId::from_bytes([4; 16]),
        session_id,
        expected_revision: 0,
        change: SteeringChange::Enqueue {
            run_id,
            text: "literal @skill text".into(),
        },
    };
    let cursor = morons_protocol::SteeringCursor {
        session_id,
        sequence: 0,
    };
    let ApplicationOutcome::SteeringSubscription(subscription) = app
        .execute_for_local_owner(Request::SubscribeSteering { cursor })
        .await
        .unwrap()
    else {
        panic!("subscription")
    };
    // Commit before the subscriber starts reading, then lose the acknowledgment.
    let (mut client, mut server) = tokio::io::duplex(4096);
    write_client_message(
        &mut client,
        &ClientMessage::request(
            1,
            Request::MutateSteering {
                mutation: mutation.clone(),
            },
        ),
    )
    .await
    .unwrap();
    drop(client);
    assert!(
        handle_local_owner_requests(&mut server, &app)
            .await
            .is_err()
    );
    let receipt = request(
        &app,
        Request::MutateSteering {
            mutation: mutation.clone(),
        },
    )
    .await;
    assert!(
        matches!(&receipt, ServerMessage::Response { response: ApplicationResponse::SteeringMutated { receipt }, .. } if receipt.queue_revision == 1)
    );
    assert_eq!(
        receipt,
        request(
            &app,
            Request::MutateSteering {
                mutation: mutation.clone()
            }
        )
        .await
    );
    let (mut client, mut server) = tokio::io::duplex(4096);
    let reader = async {
        let ServerMessage::SteeringChanged { notice } =
            read_server_message(&mut client).await.unwrap().unwrap()
        else {
            panic!("notice")
        };
        assert_eq!(notice.revision, 1);
        let mut competing = mutation.clone();
        competing.request_id = morons_protocol::MutationRequestId::from_bytes([5; 16]);
        assert!(matches!(
            request(
                &app,
                Request::MutateSteering {
                    mutation: competing
                }
            )
            .await,
            ServerMessage::RequestFailed {
                error: morons_protocol::ApplicationError::RequestConflict,
                ..
            }
        ));
        let mut pause = mutation.clone();
        pause.request_id = morons_protocol::MutationRequestId::from_bytes([6; 16]);
        pause.expected_revision = 1;
        pause.change = SteeringChange::Pause;
        request(&app, Request::MutateSteering { mutation: pause }).await;
        let ServerMessage::SteeringChanged { notice: next } =
            read_server_message(&mut client).await.unwrap().unwrap()
        else {
            panic!("notice")
        };
        assert_eq!(next.revision, 2);
        assert!(next.cursor.sequence > notice.cursor.sequence);
        drop(client);
        notice.cursor
    };
    let (stream, cursor) = time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            stream_steering_events(&mut server, &app, subscription),
            reader
        )
    })
    .await
    .unwrap();
    stream.unwrap();
    assert!(
        matches!(request(&app, Request::ReplaySteering { cursor, limit: 1 }).await, ServerMessage::Response { response: ApplicationResponse::SteeringReplayed { page }, .. } if page.notices.len() == 1 && page.notices[0].revision == 2)
    );
    assert!(
        matches!(request(&app, Request::GetSteering { session_id }).await, ServerMessage::Response { response: ApplicationResponse::SteeringFound { snapshot }, .. } if snapshot.revision == 2 && snapshot.items.len() == 1 && snapshot.paused)
    );
    app.shutdown().await;
}

#[tokio::test]
async fn steering_cancellation_wakes_subscription_and_pauses_queue() {
    let root = TestRoot::new("steering-cancellation-notice");
    let (app, session_id, run_id) = fixture(&root).await;
    let initial_run = request(&app, Request::GetRun { session_id, run_id }).await;
    let transcript_request = Request::ListSessionTranscript {
        session_id,
        cursor: None,
        direction: morons_protocol::TranscriptPageDirection::Older,
        limit: 1,
    };
    let ServerMessage::Response {
        response:
            ApplicationResponse::SessionTranscriptListed {
                entries: initial_entries,
                ..
            },
        ..
    } = request(&app, transcript_request.clone()).await
    else {
        panic!("initial transcript")
    };
    assert_eq!(initial_entries.len(), 1);
    for (revision, change) in [
        SteeringChange::Enqueue {
            run_id,
            text: "pending literal @skill".into(),
        },
        SteeringChange::Resume { run_id },
    ]
    .into_iter()
    .enumerate()
    {
        assert!(matches!(
            request(
                &app,
                Request::MutateSteering {
                    mutation: SteeringMutation {
                        request_id: morons_protocol::MutationRequestId::from_bytes(
                            [4 + revision as u8; 16]
                        ),
                        session_id,
                        expected_revision: revision as u64,
                        change,
                    },
                }
            )
            .await,
            ServerMessage::Response {
                response: ApplicationResponse::SteeringMutated { .. },
                ..
            }
        ));
    }
    let ServerMessage::Response {
        response: ApplicationResponse::SteeringFound { snapshot },
        ..
    } = request(&app, Request::GetSteering { session_id }).await
    else {
        panic!("snapshot")
    };
    assert!(!snapshot.paused);
    assert_eq!(snapshot.items.len(), 1);
    assert_eq!(snapshot.items[0].text, "pending literal @skill");
    assert_eq!(
        initial_run,
        request(&app, Request::GetRun { session_id, run_id }).await
    );
    let ServerMessage::Response {
        response: ApplicationResponse::SessionTranscriptListed { entries, .. },
        ..
    } = request(&app, transcript_request).await
    else {
        panic!("transcript after resume")
    };
    assert_eq!(initial_entries, entries);
    let ApplicationOutcome::SteeringSubscription(mut subscription) = app
        .execute_for_local_owner(Request::SubscribeSteering {
            cursor: snapshot.cursor,
        })
        .await
        .unwrap()
    else {
        panic!("subscription")
    };
    subscription.notifications.borrow_and_update();
    assert!(matches!(
        request(
            &app,
            Request::CancelRun {
                mutation_request_id: morons_protocol::MutationRequestId::from_bytes([6; 16]),
                session_id,
                run_id,
            }
        )
        .await,
        ServerMessage::Response {
            response: ApplicationResponse::RunCancellationResolved {
                cancellation_requested: true,
                ..
            },
            ..
        }
    ));
    time::timeout(
        Duration::from_secs(10),
        subscription.notifications.changed(),
    )
    .await
    .unwrap()
    .unwrap();
    let page = app
        .read_steering_events(snapshot.cursor, 128)
        .await
        .unwrap();
    assert_eq!(page.notices.len(), 1);
    assert_eq!(page.notices[0].revision, snapshot.revision + 1);
    assert!(page.notices[0].cursor.sequence > snapshot.cursor.sequence);
    assert!(
        matches!(request(&app, Request::GetSteering { session_id }).await,
        ServerMessage::Response { response: ApplicationResponse::SteeringFound { snapshot }, .. }
        if snapshot.paused && snapshot.revision == 3 && snapshot.items.len() == 1
            && snapshot.items[0].text == "pending literal @skill")
    );
    app.shutdown().await;
}

#[tokio::test]
async fn steering_terminal_run_wakes_stream_and_pauses_queue() {
    let root = TestRoot::new("steering-terminal-notice");
    let (store, session_id, run_id) = store_fixture(&root).await;
    let store = std::sync::Arc::new(store);
    let app = ServerApplication::from_native_shared_for_test(store.clone(), "http://127.0.0.1:1");
    for (revision, change) in [
        SteeringChange::Enqueue {
            run_id,
            text: "pending".into(),
        },
        SteeringChange::Resume { run_id },
    ]
    .into_iter()
    .enumerate()
    {
        assert!(matches!(
            request(
                &app,
                Request::MutateSteering {
                    mutation: SteeringMutation {
                        request_id: morons_protocol::MutationRequestId::from_bytes(
                            [4 + revision as u8; 16]
                        ),
                        session_id,
                        expected_revision: revision as u64,
                        change,
                    },
                }
            )
            .await,
            ServerMessage::Response {
                response: ApplicationResponse::SteeringMutated { .. },
                ..
            }
        ));
    }
    let ServerMessage::Response {
        response: ApplicationResponse::SteeringFound { snapshot },
        ..
    } = request(&app, Request::GetSteering { session_id }).await
    else {
        panic!("snapshot")
    };
    let ApplicationOutcome::SteeringSubscription(mut subscription) = app
        .execute_for_local_owner(Request::SubscribeSteering {
            cursor: snapshot.cursor,
        })
        .await
        .unwrap()
    else {
        panic!("subscription")
    };
    subscription.notifications.borrow_and_update();
    store
        .finish_run_stopped(
            crate::persistence::RunId::from_bytes(*run_id.as_bytes()),
            None,
        )
        .await
        .unwrap();
    time::timeout(
        Duration::from_secs(10),
        subscription.notifications.changed(),
    )
    .await
    .unwrap()
    .unwrap();
    let (mut client, mut server) = tokio::io::duplex(4096);
    let reader = async {
        let ServerMessage::SteeringChanged { notice } =
            read_server_message(&mut client).await.unwrap().unwrap()
        else {
            panic!("terminal notice")
        };
        assert_eq!(notice.revision, 3);
        assert!(notice.cursor.sequence > snapshot.cursor.sequence);
        assert!(
            matches!(request(&app, Request::GetSteering { session_id }).await,
            ServerMessage::Response { response: ApplicationResponse::SteeringFound { snapshot }, .. }
            if snapshot.paused && snapshot.revision == 3 && snapshot.items.len() == 1)
        );
        drop(client);
    };
    let (result, ()) = time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            stream_steering_events(&mut server, &app, subscription),
            reader
        )
    })
    .await
    .unwrap();
    result.unwrap();
    app.shutdown().await;
}

#[tokio::test]
async fn steering_backlog_observes_client_messages_and_live_write_timeout() {
    let root = TestRoot::new("steering-backlog-client");
    let (app, session_id, run_id) = fixture(&root).await;
    assert!(matches!(
        request(
            &app,
            Request::MutateSteering {
                mutation: SteeringMutation {
                    request_id: morons_protocol::MutationRequestId::from_bytes([4; 16]),
                    session_id,
                    expected_revision: 0,
                    change: SteeringChange::Enqueue {
                        run_id,
                        text: "pending".into()
                    },
                },
            }
        )
        .await,
        ServerMessage::Response {
            response: ApplicationResponse::SteeringMutated { .. },
            ..
        }
    ));
    for send_invalid in [true, false] {
        let ApplicationOutcome::SteeringSubscription(subscription) = app
            .execute_for_local_owner(Request::SubscribeSteering {
                cursor: morons_protocol::SteeringCursor {
                    session_id,
                    sequence: 0,
                },
            })
            .await
            .unwrap()
        else {
            panic!("subscription")
        };
        let (mut client, mut server) = tokio::io::duplex(1);
        let client_work = async {
            if send_invalid {
                write_client_message(
                    &mut client,
                    &ClientMessage::request(2, Request::GetSteering { session_id }),
                )
                .await
                .unwrap();
            }
            time::sleep(Duration::from_secs(1)).await;
        };
        let (result, ()) = tokio::join!(
            stream_steering_events(&mut server, &app, subscription),
            client_work
        );
        if send_invalid {
            assert!(matches!(
                result,
                Err(ConnectionError::UnexpectedClientMessage)
            ));
        } else {
            assert!(matches!(
                result,
                Err(ConnectionError::SubscriptionWriteTimedOut)
            ));
        }
    }
    app.shutdown().await;
}

#[tokio::test]
async fn steering_concurrent_snapshot_and_subscription_preserve_enqueue() {
    let root = TestRoot::new("steering-concurrent-boundary");
    let (app, session_id, run_id) = fixture(&root).await;
    let cursor = morons_protocol::SteeringCursor {
        session_id,
        sequence: 0,
    };
    let (snapshot, subscription, mutation) = tokio::join!(
        request(&app, Request::GetSteering { session_id }),
        app.execute_for_local_owner(Request::SubscribeSteering { cursor }),
        request(
            &app,
            Request::MutateSteering {
                mutation: SteeringMutation {
                    request_id: morons_protocol::MutationRequestId::from_bytes([4; 16]),
                    session_id,
                    expected_revision: 0,
                    change: SteeringChange::Enqueue {
                        run_id,
                        text: "literal @skill".into(),
                    },
                },
            },
        ),
    );
    let ServerMessage::Response {
        response: ApplicationResponse::SteeringMutated { receipt },
        ..
    } = mutation
    else {
        panic!("mutation")
    };
    let ServerMessage::Response {
        response: ApplicationResponse::SteeringFound { snapshot },
        ..
    } = snapshot
    else {
        panic!("snapshot")
    };
    let page = app
        .read_steering_events(snapshot.cursor, 128)
        .await
        .unwrap();
    assert_eq!(snapshot.items.len() + page.notices.len(), 1);
    assert_eq!(snapshot.revision, snapshot.items.len() as u64);
    assert_eq!(page.high_water.sequence, receipt.sequence);
    if snapshot.items.is_empty() {
        assert_eq!(snapshot.cursor, cursor);
        assert_eq!(page.notices[0].revision, receipt.queue_revision);
    } else {
        assert_eq!(snapshot.cursor.sequence, receipt.sequence);
        assert_eq!(snapshot.items[0].text, "literal @skill");
    }
    let ApplicationOutcome::SteeringSubscription(subscription) = subscription.unwrap() else {
        panic!("subscription")
    };
    let (mut client, mut server) = tokio::io::duplex(4096);
    let reader = async {
        assert!(matches!(
            read_server_message(&mut client).await.unwrap().unwrap(),
            ServerMessage::SteeringChanged { notice }
                if notice.cursor.session_id == session_id
                    && notice.cursor.sequence == receipt.sequence
                    && notice.revision == receipt.queue_revision
        ));
        drop(client);
    };
    let (result, ()) = time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            stream_steering_events(&mut server, &app, subscription),
            reader
        )
    })
    .await
    .unwrap();
    result.unwrap();
    app.shutdown().await;
}

#[tokio::test]
async fn steering_snapshot_boundary_and_replay_limits() {
    let root = TestRoot::new("steering-snapshot-boundary");
    let (app, session_id, run_id) = fixture(&root).await;
    let ServerMessage::Response {
        response: ApplicationResponse::SteeringFound { snapshot },
        ..
    } = request(&app, Request::GetSteering { session_id }).await
    else {
        panic!("snapshot")
    };
    assert!(matches!(
        request(
            &app,
            Request::MutateSteering {
                mutation: SteeringMutation {
                    request_id: morons_protocol::MutationRequestId::from_bytes([4; 16]),
                    session_id,
                    expected_revision: snapshot.revision,
                    change: SteeringChange::Enqueue {
                        run_id,
                        text: "pending".into()
                    },
                },
            }
        )
        .await,
        ServerMessage::Response {
            response: ApplicationResponse::SteeringMutated { .. },
            ..
        }
    ));
    for limit in [0, 129, u16::MAX] {
        assert!(matches!(
            request(
                &app,
                Request::ReplaySteering {
                    cursor: snapshot.cursor,
                    limit
                }
            )
            .await,
            ServerMessage::RequestFailed {
                error: morons_protocol::ApplicationError::InvalidRequest,
                ..
            }
        ));
    }
    let (mut client, mut server) = tokio::io::duplex(4096);
    let client_work = async {
        write_client_message(
            &mut client,
            &ClientMessage::request(
                1,
                Request::SubscribeSteering {
                    cursor: snapshot.cursor,
                },
            ),
        )
        .await
        .unwrap();
        assert!(matches!(
            read_server_message(&mut client).await.unwrap().unwrap(),
            ServerMessage::Response {
                response: ApplicationResponse::SteeringSubscriptionStarted { .. },
                ..
            }
        ));
        assert!(
            matches!(read_server_message(&mut client).await.unwrap().unwrap(),
            ServerMessage::SteeringChanged { notice } if notice.revision == snapshot.revision + 1)
        );
        drop(client);
    };
    let (result, ()) = time::timeout(Duration::from_secs(10), async {
        tokio::join!(handle_local_owner_requests(&mut server, &app), client_work)
    })
    .await
    .unwrap();
    result.unwrap();
    app.shutdown().await;
}

async fn create_idle_session(
    app: &ServerApplication,
    root: &TestRoot,
) -> morons_protocol::SessionId {
    let ServerMessage::Response {
        response: ApplicationResponse::SessionCreated { session },
        ..
    } = request(
        app,
        Request::CreateSession {
            mutation_request_id: morons_protocol::MutationRequestId::from_bytes([200; 16]),
            display_name: None,
            working_directory: root.path().to_str().unwrap().into(),
        },
    )
    .await
    else {
        panic!("created session")
    };
    session.id
}

#[tokio::test]
async fn steering_subscription_ends_when_idle_session_is_deleted() {
    let root = TestRoot::new("steering-delete");
    let (app, _, _) = fixture(&root).await;
    let session_id = create_idle_session(&app, &root).await;
    let (mut client, mut server) = tokio::io::duplex(4096);
    let client_work = async {
        write_client_message(
            &mut client,
            &ClientMessage::request(
                1,
                Request::SubscribeSteering {
                    cursor: morons_protocol::SteeringCursor {
                        session_id,
                        sequence: 0,
                    },
                },
            ),
        )
        .await
        .unwrap();
        assert!(matches!(
            read_server_message(&mut client).await.unwrap().unwrap(),
            ServerMessage::Response {
                response: ApplicationResponse::SteeringSubscriptionStarted { .. },
                ..
            }
        ));
        for operation in [
            Request::SetSessionArchived {
                mutation_request_id: morons_protocol::MutationRequestId::from_bytes([201; 16]),
                session_id,
                archived: true,
            },
            Request::DeleteSession {
                mutation_request_id: morons_protocol::MutationRequestId::from_bytes([202; 16]),
                session_id,
            },
        ] {
            assert!(matches!(
                request(&app, operation).await,
                ServerMessage::Response { .. }
            ));
        }
        assert!(matches!(
            read_server_message(&mut client).await.unwrap().unwrap(),
            ServerMessage::SubscriptionEnded {
                error: morons_protocol::ApplicationError::SessionNotFound
            }
        ));
    };
    let (result, ()) = time::timeout(Duration::from_secs(10), async {
        tokio::join!(handle_local_owner_requests(&mut server, &app), client_work)
    })
    .await
    .unwrap();
    result.unwrap();
    app.shutdown().await;
}

#[tokio::test]
async fn steering_reconnect_pages_are_bounded_and_session_isolated() {
    let root = TestRoot::new("steering-pages");
    let (app, session_id, run_id) = fixture(&root).await;
    let other_session = create_idle_session(&app, &root).await;
    let mut item_id = None;
    for revision in 0..130u64 {
        let change = match item_id {
            None => SteeringChange::Enqueue {
                run_id,
                text: "literal @skill".into(),
            },
            Some(item_id) => SteeringChange::Edit {
                item_id,
                revision,
                text: format!("literal @skill {revision}"),
            },
        };
        let ServerMessage::Response {
            response: ApplicationResponse::SteeringMutated { receipt },
            ..
        } = request(
            &app,
            Request::MutateSteering {
                mutation: SteeringMutation {
                    request_id: morons_protocol::MutationRequestId::from_bytes(
                        (1000 + u128::from(revision)).to_le_bytes(),
                    ),
                    session_id,
                    expected_revision: revision,
                    change,
                },
            },
        )
        .await
        else {
            panic!("mutation")
        };
        assert_eq!(receipt.queue_revision, revision + 1);
        item_id = receipt.item_id;
    }
    let mut cursor = morons_protocol::SteeringCursor {
        session_id,
        sequence: 0,
    };
    for (count, first_revision) in [(128, 1), (2, 129), (0, 131)] {
        let ServerMessage::Response {
            response: ApplicationResponse::SteeringReplayed { page },
            ..
        } = request(&app, Request::ReplaySteering { cursor, limit: 128 }).await
        else {
            panic!("page")
        };
        assert_eq!(page.notices.len(), count);
        for (index, notice) in page.notices.iter().enumerate() {
            assert_eq!(notice.cursor.session_id, session_id);
            assert_eq!(notice.revision, first_revision + index as u64);
            assert!(notice.cursor.sequence > cursor.sequence);
            cursor = notice.cursor;
        }
        if count == 128 {
            assert!(cursor.sequence < page.high_water.sequence);
        } else {
            assert_eq!(cursor, page.high_water);
        }
    }
    for received in [130, 129] {
        let (mut client, mut server) = tokio::io::duplex(256);
        let client_work = async {
            let start = morons_protocol::SteeringCursor {
                session_id,
                sequence: 0,
            };
            write_client_message(
                &mut client,
                &ClientMessage::request(1, Request::SubscribeSteering { cursor: start }),
            )
            .await
            .unwrap();
            assert!(matches!(
                read_server_message(&mut client).await.unwrap().unwrap(),
                ServerMessage::Response {
                    response: ApplicationResponse::SteeringSubscriptionStarted { cursor },
                    ..
                } if cursor == start
            ));
            let mut last = start;
            for revision in 1..=received {
                let ServerMessage::SteeringChanged { notice } =
                    read_server_message(&mut client).await.unwrap().unwrap()
                else {
                    panic!("streamed notice")
                };
                assert_eq!(notice.revision, revision);
                assert_eq!(notice.cursor.session_id, session_id);
                assert!(notice.cursor.sequence > last.sequence);
                last = notice.cursor;
            }
            if received == 130 {
                assert_eq!(last, cursor);
            }
            // Half-close input so EOF interrupts replay without a racing broken pipe.
            client.shutdown().await.unwrap();
            client
        };
        let (result, _client) = time::timeout(Duration::from_secs(10), async {
            tokio::join!(handle_local_owner_requests(&mut server, &app), client_work)
        })
        .await
        .unwrap();
        result.unwrap();
    }
    assert!(matches!(request(&app, Request::ReplaySteering {
        cursor: morons_protocol::SteeringCursor { session_id: other_session, sequence: 0 }, limit: 128,
    }).await, ServerMessage::Response {
        response: ApplicationResponse::SteeringReplayed { page }, ..
    } if page.notices.is_empty() && page.high_water.sequence == 0));
    assert!(
        matches!(request(&app, Request::GetSteering { session_id: other_session }).await,
        ServerMessage::Response { response: ApplicationResponse::SteeringFound { snapshot }, .. }
        if snapshot.items.is_empty() && snapshot.revision == 0 && snapshot.target_run_id.is_none())
    );
    app.shutdown().await;
}

#[tokio::test]
async fn steering_subscription_rejects_future_cursor_and_disconnects_slow_writer() {
    let root = TestRoot::new("steering-slow-writer");
    let (app, session_id, _) = fixture(&root).await;
    assert!(matches!(
        request(
            &app,
            Request::SubscribeSteering {
                cursor: morons_protocol::SteeringCursor {
                    session_id,
                    sequence: 1
                }
            }
        )
        .await,
        ServerMessage::RequestFailed {
            error: morons_protocol::ApplicationError::InvalidRequest,
            ..
        }
    ));
    let (mut client, mut server) = tokio::io::duplex(1);
    let send = async {
        write_client_message(
            &mut client,
            &ClientMessage::request(
                1,
                Request::SubscribeSteering {
                    cursor: morons_protocol::SteeringCursor {
                        session_id,
                        sequence: 0,
                    },
                },
            ),
        )
        .await
        .unwrap();
        time::sleep(Duration::from_secs(1)).await;
    };
    let (result, ()) = tokio::join!(handle_local_owner_requests(&mut server, &app), send);
    assert!(matches!(
        result,
        Err(ConnectionError::SubscriptionWriteTimedOut)
    ));
    app.shutdown().await;
}
