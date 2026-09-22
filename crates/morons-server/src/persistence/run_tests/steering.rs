mod delivery;

use super::*;
use crate::persistence::{ProviderOperationFailureState, RunFailureKind};

#[tokio::test(flavor = "current_thread")]
async fn steering_pending_input_cannot_replace_initiating_message_on_restart() {
    for corruption in [
        "UPDATE session_entries SET text = 'Queued' WHERE message_id = ?1",
        "UPDATE session_entries SET message_id = randomblob(16) WHERE message_id = ?1",
        "UPDATE run_input_requests SET user_message_id = randomblob(16) WHERE user_message_id = ?1",
        "UPDATE run_accepted_facts SET user_message_id = randomblob(16) WHERE user_message_id = ?1",
    ] {
        assert_steering_initiating_message_corruption_rejected(corruption).await;
    }
}

async fn assert_steering_initiating_message_corruption_rejected(corruption: &str) {
    use crate::persistence::steering::{SteeringChange, SteeringMutation};

    let root = TestRoot::new("steering-initiating-message");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    configure_credential(&store).await;
    let session = store
        .create_session(MutationRequestId::from_bytes([0xe1; 16]), None)
        .await
        .unwrap();
    let run = store
        .accept_session_input(
            MutationRequestId::from_bytes([0xe2; 16]),
            session.id,
            "Initial".into(),
            model_selection(),
        )
        .await
        .unwrap()
        .run;
    store
        .mutate_steering(SteeringMutation {
            request_id: MutationRequestId::from_bytes([0xe3; 16]),
            session_id: session.id,
            expected_revision: 0,
            change: SteeringChange::Enqueue {
                run_id: run.id,
                text: "Queued".into(),
            },
        })
        .await
        .unwrap();
    drop(store);
    let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    assert_eq!(
        db.execute(corruption, [run.user_message_id.as_bytes()])
            .unwrap(),
        1
    );
    drop(db);
    assert!(
        matches!(
            SessionStore::open_for_test(root.path()),
            Err(PersistenceError::InvalidState { .. })
        ),
        "accepted initiating-message corruption: {corruption}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn steering_provider_completion_pauses_once_and_preserves_pending_input() {
    use crate::persistence::steering::{SteeringChange, SteeringMutation};

    for operation_state in [
        None,
        Some(ProviderOperationFailureState::Failed),
        Some(ProviderOperationFailureState::Uncertain),
    ] {
        let root = TestRoot::new("steering-provider-completion");
        let store = SessionStore::open_for_test(root.path()).unwrap();
        configure_credential(&store).await;
        let session = store
            .create_session(MutationRequestId::from_bytes([0xe1; 16]), None)
            .await
            .unwrap();
        let run = store
            .accept_session_input(
                MutationRequestId::from_bytes([0xe2; 16]),
                session.id,
                "Initial".into(),
                model_selection(),
            )
            .await
            .unwrap()
            .run;
        assert_eq!(
            store.activate_run(run.id).await.unwrap(),
            ActivationOutcome::Active
        );
        let context = store.load_run_context(run.id).await.unwrap();
        let operation = match store
            .prepare_provider_operation(
                run.id,
                context.current_entry_high_water,
                context.estimated_input_tokens,
            )
            .await
            .unwrap()
        {
            PrepareOperationOutcome::Prepared(operation) => operation,
            other => panic!("unexpected preparation outcome: {other:?}"),
        };
        assert_eq!(
            store
                .mark_provider_dispatched(run.id, operation)
                .await
                .unwrap(),
            DispatchOutcome::Dispatched
        );
        let enqueue = SteeringMutation {
            request_id: MutationRequestId::from_bytes([3; 16]),
            session_id: session.id,
            expected_revision: 0,
            change: SteeringChange::Enqueue {
                run_id: run.id,
                text: "Retain literal @skill".into(),
            },
        };
        let receipt = store.mutate_steering(enqueue.clone()).await.unwrap();
        store
            .mutate_steering(SteeringMutation {
                request_id: MutationRequestId::from_bytes([4; 16]),
                session_id: session.id,
                expected_revision: 1,
                change: SteeringChange::Resume { run_id: run.id },
            })
            .await
            .unwrap();
        let before = store.steering_snapshot(session.id).await.unwrap();
        assert!(!before.paused);
        let terminal = match operation_state {
            Some(operation_state) => {
                let terminal = store
                    .finish_run_failure(
                        run.id,
                        Some(operation),
                        RunFailureKind::ProviderUnavailable,
                        operation_state,
                    )
                    .await
                    .unwrap();
                assert_eq!(terminal.state, RunState::Failed);
                let context = store.load_run_context(run.id).await.unwrap();
                assert!(matches!(
                    &context.entries[..],
                    [TranscriptEntry::UserMessage { text, .. }] if text == "Initial"
                ));
                terminal
            }
            None => {
                let terminal = store
                    .complete_run_success(
                        run.id,
                        operation,
                        CompletedAssistant {
                            text: "durable answer".into(),
                            refusal: false,
                            provider_response_id: "resp_test".into(),
                            usage: ProviderUsage {
                                input_tokens: 10,
                                cached_input_tokens: 0,
                                cache_write_input_tokens: 0,
                                output_tokens: 4,
                                reasoning_output_tokens: 0,
                                total_tokens: 14,
                            },
                        },
                    )
                    .await
                    .unwrap();
                assert_eq!(terminal.state, RunState::Succeeded);
                let context = store.load_run_context(run.id).await.unwrap();
                assert!(matches!(
                    &context.entries[..],
                    [
                        TranscriptEntry::UserMessage { text: user_text, .. },
                        TranscriptEntry::AssistantMessage { text: assistant_text, .. },
                    ] if user_text == "Initial" && assistant_text == "durable answer"
                ));
                terminal
            }
        };
        assert!(terminal.state.is_terminal());
        let after = store.steering_snapshot(session.id).await.unwrap();
        assert!(after.paused);
        assert_eq!(after.revision, 3);
        assert_eq!(after.items, before.items);
        let notices = store.steering_replay(before.cursor, 128).await.unwrap();
        assert_eq!(notices.notices.len(), 1);
        assert_eq!(notices.notices[0].cursor, after.cursor);
        assert_eq!(notices.notices[0].revision, 3);
        assert_eq!(
            store.mutate_steering(enqueue.clone()).await.unwrap(),
            receipt
        );
        assert!(matches!(
            store
                .mutate_steering(SteeringMutation {
                    request_id: MutationRequestId::from_bytes([5; 16]),
                    session_id: session.id,
                    expected_revision: 3,
                    change: SteeringChange::Resume { run_id: run.id },
                })
                .await,
            Err(PersistenceError::RequestConflict)
        ));
        store.finish_run_stopped(run.id, None).await.unwrap();
        assert_eq!(store.steering_snapshot(session.id).await.unwrap(), after);
        drop(store);
        let store = SessionStore::open_for_test(root.path()).unwrap();
        assert_eq!(store.steering_snapshot(session.id).await.unwrap(), after);
        assert_eq!(store.mutate_steering(enqueue).await.unwrap(), receipt);
        assert!(
            store
                .steering_replay(after.cursor, 128)
                .await
                .unwrap()
                .notices
                .is_empty()
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn steering_concurrent_readers_and_writers_preserve_replay_boundaries() {
    use crate::persistence::steering::{SteeringChange, SteeringMutation};
    let root = TestRoot::new("steering-concurrent-replay");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    configure_credential(&store).await;
    let mut sessions = Vec::new();
    for id in [0xe1, 0xe2] {
        let session = store
            .create_session(MutationRequestId::from_bytes([id; 16]), None)
            .await
            .unwrap();
        let run = store
            .accept_session_input(
                MutationRequestId::from_bytes([id + 2; 16]),
                session.id,
                "Initial".into(),
                model_selection(),
            )
            .await
            .unwrap()
            .run;
        sessions.push((session.id, run.id));
    }
    let (session_id, run_id) = sessions[0];
    let initial = store.steering_snapshot(session_id).await.unwrap();
    for revision in 0..8 {
        let mutation = |id, session_id, run_id| SteeringMutation {
            request_id: MutationRequestId::from_bytes([id; 16]),
            session_id,
            expected_revision: revision,
            change: SteeringChange::Enqueue {
                run_id,
                text: format!("Pending {revision}"),
            },
        };
        let (accepted, competing, other, reader_a, reader_b) = tokio::join!(
            store.mutate_steering(mutation(10 + revision as u8, session_id, run_id)),
            store.mutate_steering(mutation(30 + revision as u8, session_id, run_id)),
            store.mutate_steering(mutation(50 + revision as u8, sessions[1].0, sessions[1].1)),
            store.steering_snapshot(session_id),
            store.steering_snapshot(session_id),
        );
        assert_ne!(accepted.is_ok(), competing.is_ok());
        other.unwrap();
        let winner = accepted.or(competing).unwrap();
        for snapshot in [reader_a.unwrap(), reader_b.unwrap()] {
            assert_eq!(snapshot.items.len() as u64, snapshot.revision);
            assert!([revision, revision + 1].contains(&snapshot.revision));
            let page = store.steering_replay(snapshot.cursor, 128).await.unwrap();
            assert_eq!(page.notices.len() as u64, revision + 1 - snapshot.revision);
            assert_eq!(page.high_water.sequence, winner.sequence);
            if let Some(notice) = page.notices.first() {
                assert_eq!(notice.revision, snapshot.revision + 1);
            }
        }
    }
    // Independent slow readers must not skip pages when the high-water advances.
    for limit in [1, 3] {
        let mut cursor = initial.cursor;
        let mut revisions = Vec::new();
        loop {
            let page = store.steering_replay(cursor, limit).await.unwrap();
            for notice in &page.notices {
                assert_eq!(notice.cursor.session_id, session_id);
                assert!(notice.cursor.sequence > cursor.sequence);
                cursor = notice.cursor;
                revisions.push(notice.revision);
            }
            if cursor == page.high_water {
                break;
            }
            assert!(!page.notices.is_empty());
        }
        assert_eq!(revisions, (1..=8).collect::<Vec<_>>());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn steering_snapshots_and_paginated_notices_share_a_durable_boundary() {
    use crate::persistence::steering::{SteeringChange, SteeringCursor, SteeringMutation};
    let root = TestRoot::new("steering-replay");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    configure_credential(&store).await;
    let session = store
        .create_session(MutationRequestId::from_bytes([0xe1; 16]), None)
        .await
        .unwrap();
    let other = store
        .create_session(MutationRequestId::from_bytes([0xe2; 16]), None)
        .await
        .unwrap();
    let initial = store.steering_snapshot(session.id).await.unwrap();
    assert_eq!(initial.revision, 0);
    assert!(initial.paused && initial.items.is_empty() && initial.target_run_id.is_none());
    let run = store
        .accept_session_input(
            MutationRequestId::from_bytes([3; 16]),
            session.id,
            "Initial".into(),
            model_selection(),
        )
        .await
        .unwrap()
        .run;
    let mutation = |id, expected_revision, change| SteeringMutation {
        request_id: MutationRequestId::from_bytes([id; 16]),
        session_id: session.id,
        expected_revision,
        change,
    };
    let enqueue = mutation(
        4,
        0,
        SteeringChange::Enqueue {
            run_id: run.id,
            text: "Pending".into(),
        },
    );
    let receipt = store.mutate_steering(enqueue.clone()).await.unwrap();
    let snapshot = store.steering_snapshot(session.id).await.unwrap();
    assert_eq!(snapshot.cursor.sequence, receipt.sequence);
    assert_eq!(snapshot.items[0].text, "Pending");
    store
        .mutate_steering(mutation(
            5,
            1,
            SteeringChange::Edit {
                item_id: receipt.item_id.unwrap(),
                revision: 1,
                text: "Edited".into(),
            },
        ))
        .await
        .unwrap();
    store
        .mutate_steering(mutation(6, 2, SteeringChange::Resume { run_id: run.id }))
        .await
        .unwrap();
    assert_eq!(store.mutate_steering(enqueue).await.unwrap(), receipt);
    let first = store.steering_replay(snapshot.cursor, 1).await.unwrap();
    assert_eq!(first.notices.len(), 1);
    assert_eq!(first.notices[0].revision, 2);
    assert!(first.high_water.sequence > first.notices[0].cursor.sequence);
    let second = store
        .steering_replay(first.notices[0].cursor, 1)
        .await
        .unwrap();
    assert_eq!(second.notices[0].revision, 3);
    assert_eq!(second.notices[0].cursor, second.high_water);
    assert!(
        store
            .steering_replay(second.high_water, 1)
            .await
            .unwrap()
            .notices
            .is_empty()
    );
    assert!(
        store
            .steering_replay(
                SteeringCursor {
                    session_id: other.id,
                    sequence: 0
                },
                128
            )
            .await
            .unwrap()
            .notices
            .is_empty()
    );
    for limit in [0, 129] {
        assert!(matches!(
            store.steering_replay(initial.cursor, limit).await,
            Err(PersistenceError::InvalidInput { .. })
        ));
    }
    assert!(matches!(
        store
            .steering_replay(
                SteeringCursor {
                    sequence: u64::MAX,
                    ..initial.cursor
                },
                1
            )
            .await,
        Err(PersistenceError::InvalidInput { .. })
    ));
    drop(store);
    let store = SessionStore::open_for_test(root.path()).unwrap();
    let recovered = store.steering_snapshot(session.id).await.unwrap();
    assert!(recovered.paused);
    assert_eq!(recovered.revision, 4);
    assert_eq!(recovered.items[0].text, "Edited");
    assert_eq!(recovered.items[0].enqueue_sequence, receipt.sequence);
    let recovery = store.steering_replay(second.high_water, 128).await.unwrap();
    assert_eq!(recovery.notices.len(), 1);
    assert_eq!(recovery.notices[0].revision, 4);
    assert_eq!(recovery.high_water, recovered.cursor);
    drop(store);
    let store = SessionStore::open_for_test(root.path()).unwrap();
    assert_eq!(
        store.steering_snapshot(session.id).await.unwrap(),
        recovered
    );
    assert!(
        store
            .steering_replay(recovered.cursor, 128)
            .await
            .unwrap()
            .notices
            .is_empty()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn steering_queue_cancel_intent_and_interruption_pause_without_consumption() {
    for cancel in [false, true] {
        let root = TestRoot::new("steering-stop");
        let store = SessionStore::open_for_test(root.path()).unwrap();
        configure_credential(&store).await;
        let session = store
            .create_session(MutationRequestId::from_bytes([0xb1; 16]), None)
            .await
            .unwrap();
        let accepted = store
            .accept_session_input(
                MutationRequestId::from_bytes([0xb2; 16]),
                session.id,
                "Initial".into(),
                model_selection(),
            )
            .await
            .unwrap();
        let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        db.execute(
            "INSERT INTO steering_queues (session_id, target_run_id, revision, paused)
             VALUES (?1, ?2, 1, 0)",
            rusqlite::params![session.id.as_bytes(), accepted.run.id.as_bytes()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO steering_pending_messages
             VALUES (zeroblob(16), ?1, 1, 1, 1, 'Pending', 1, 0)",
            [session.id.as_bytes()],
        )
        .unwrap();
        let state = || {
            db.query_row("SELECT paused, revision FROM steering_queues", [], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .unwrap()
        };
        store.activate_run(accepted.run.id).await.unwrap();
        assert_eq!(state(), (0, 1));
        if cancel {
            let request = MutationRequestId::from_bytes([0xb3; 16]);
            db.execute_batch(
                "CREATE TRIGGER reject_steering_pause BEFORE INSERT ON steering_lifecycle_facts
                 BEGIN SELECT RAISE(ABORT, 'injected pause failure'); END;",
            )
            .unwrap();
            assert!(
                store
                    .cancel_run(request, session.id, accepted.run.id)
                    .await
                    .is_err()
            );
            assert_eq!(state(), (0, 1));
            assert_eq!(
                db.query_row(
                    "SELECT COUNT(*) FROM run_cancellation_requests",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
                0
            );
            db.execute_batch("DROP TRIGGER reject_steering_pause")
                .unwrap();
            let result = store
                .cancel_run(request, session.id, accepted.run.id)
                .await
                .unwrap();
            assert!(result.intent_applied);
            assert_eq!(state(), (1, 2), "pause at intent, not only after stopping");
            assert_eq!(
                store
                    .cancel_run(request, session.id, accepted.run.id)
                    .await
                    .unwrap(),
                result
            );
            assert_eq!(state(), (1, 2));
        }
        let run = store
            .finish_run_stopped(accepted.run.id, None)
            .await
            .unwrap();
        assert_eq!(
            run.state,
            if cancel {
                RunState::Cancelled
            } else {
                RunState::Interrupted
            }
        );
        assert_eq!(state(), (1, 2));
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*), reason, queue_revision FROM steering_lifecycle_facts",
                [],
                |row| Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?
                )),
            )
            .unwrap(),
            (1, if cancel { 1 } else { 2 }, 2),
        );
        store
            .finish_run_stopped(accepted.run.id, None)
            .await
            .unwrap();
        assert_eq!(state(), (1, 2));
        assert_eq!(
            db.query_row("SELECT text FROM steering_pending_messages", [], |r| r
                .get::<_, String>(
                0
            ))
            .unwrap(),
            "Pending"
        );
        drop(db);
        drop(store);
        drop(SessionStore::open_for_test(root.path()).unwrap());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn steering_queue_restart_archive_and_deletion_preserve_lifecycle_boundaries() {
    let root = TestRoot::new("steering-lifecycle");
    let selected = TestRoot::new("steering-selected");
    let sentinel = selected.path().join("keep.txt");
    fs::write(&sentinel, "keep").unwrap();
    let store = SessionStore::open_for_test(root.path()).unwrap();
    configure_credential(&store).await;
    let session = store
        .create_session_at(
            MutationRequestId::from_bytes([0xa1; 16]),
            None,
            selected.path().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    let accepted = store
        .accept_session_input(
            MutationRequestId::from_bytes([0xa2; 16]),
            session.id,
            "Initial input".into(),
            model_selection(),
        )
        .await
        .unwrap();
    drop(store);
    let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    db.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    db.execute(
        "INSERT INTO steering_queues (session_id, target_run_id, revision, paused)
         VALUES (?1, ?2, 1, 0)",
        rusqlite::params![session.id.as_bytes(), accepted.run.id.as_bytes()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO steering_pending_messages
         (item_id, session_id, slot, enqueue_sequence, revision, text, actor, created_at_milliseconds)
         VALUES (zeroblob(16), ?1, 1, 1, 1, 'Pending only', 1, 0)",
        [session.id.as_bytes()],
    )
    .unwrap();
    let state = || {
        db.query_row("SELECT paused, revision FROM steering_queues", [], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .unwrap()
    };
    let store = SessionStore::open_for_test(root.path()).unwrap();
    assert_eq!(state(), (1, 2));
    drop(store);
    let store = SessionStore::open_for_test(root.path()).unwrap();
    assert_eq!(
        state(),
        (1, 2),
        "repeated recovery must not change paused queues"
    );
    assert_eq!(
        db.query_row("SELECT text FROM steering_pending_messages", [], |r| r
            .get::<_, String>(
            0
        ))
        .unwrap(),
        "Pending only"
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM run_accepted_facts", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1,
        "recovery must not start a follow-up run"
    );
    db.execute("UPDATE steering_queues SET paused = 0", [])
        .unwrap();
    store
        .set_session_archived(MutationRequestId::from_bytes([0xa3; 16]), session.id, true)
        .await
        .unwrap();
    assert_eq!(state(), (1, 3));
    store
        .set_session_archived(MutationRequestId::from_bytes([0xa4; 16]), session.id, false)
        .await
        .unwrap();
    assert_eq!(state(), (1, 3), "unarchiving must not resume delivery");
    store
        .set_session_archived(MutationRequestId::from_bytes([0xa5; 16]), session.id, true)
        .await
        .unwrap();
    store
        .delete_session(MutationRequestId::from_bytes([0xa6; 16]), session.id)
        .await
        .unwrap();
    for table in ["steering_queues", "steering_pending_messages"] {
        assert_eq!(
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    assert_eq!(fs::read_to_string(sentinel).unwrap(), "keep");
}

#[tokio::test(flavor = "current_thread")]
async fn steering_mutations_retry_conflict_and_preserve_pending_text() {
    use crate::persistence::steering::{SteeringChange, SteeringMutation};
    let root = TestRoot::new("steering-mutations");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    configure_credential(&store).await;
    let session = store
        .create_session(MutationRequestId::from_bytes([0xc1; 16]), None)
        .await
        .unwrap();
    let run = store
        .accept_session_input(
            MutationRequestId::from_bytes([2; 16]),
            session.id,
            "Initial".into(),
            model_selection(),
        )
        .await
        .unwrap()
        .run;
    let mutation = |id, expected_revision, change| SteeringMutation {
        request_id: MutationRequestId::from_bytes([id; 16]),
        session_id: session.id,
        expected_revision,
        change,
    };
    for text in [
        "".into(),
        " !pwd".into(),
        "\n/compact".into(),
        "é".repeat(32769),
    ] {
        assert!(matches!(
            store
                .mutate_steering(mutation(
                    3,
                    0,
                    SteeringChange::Enqueue {
                        run_id: run.id,
                        text,
                    }
                ))
                .await,
            Err(PersistenceError::InvalidInput { .. })
        ));
    }
    for id in [0, 2] {
        assert!(
            store
                .mutate_steering(mutation(
                    id,
                    0,
                    SteeringChange::Enqueue {
                        run_id: run.id,
                        text: "Rejected identity".into(),
                    }
                ))
                .await
                .is_err()
        );
    }
    let enqueue = mutation(
        3,
        0,
        SteeringChange::Enqueue {
            run_id: run.id,
            text: "Queued".into(),
        },
    );
    let receipt = store.mutate_steering(enqueue.clone()).await.unwrap();
    assert_eq!(receipt.queue_revision, 1);
    assert!(matches!(
        store
            .mutate_steering(mutation(
                4,
                1,
                SteeringChange::Edit {
                    item_id: receipt.item_id.unwrap(),
                    revision: 1,
                    text: "!!pwd".into(),
                }
            ))
            .await,
        Err(PersistenceError::InvalidInput { .. })
    ));
    assert_eq!(
        store.mutate_steering(enqueue.clone()).await.unwrap(),
        receipt
    );
    let edited = store
        .mutate_steering(mutation(
            4,
            1,
            SteeringChange::Edit {
                item_id: receipt.item_id.unwrap(),
                revision: 1,
                text: "Edited".into(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(edited.item_revision, Some(2));
    assert_eq!(
        store.mutate_steering(enqueue.clone()).await.unwrap(),
        receipt
    );
    assert!(matches!(
        store
            .mutate_steering(mutation(4, 2, SteeringChange::Pause))
            .await,
        Err(PersistenceError::RequestConflict)
    ));
    assert!(matches!(
        store
            .mutate_steering(mutation(5, 1, SteeringChange::Pause))
            .await,
        Err(PersistenceError::RequestConflict)
    ));
    store
        .mutate_steering(mutation(6, 2, SteeringChange::Resume { run_id: run.id }))
        .await
        .unwrap();
    store
        .mutate_steering(mutation(7, 3, SteeringChange::Pause))
        .await
        .unwrap();
    assert!(matches!(
        store
            .mutate_steering(mutation(
                8,
                4,
                SteeringChange::Remove {
                    item_id: receipt.item_id.unwrap(),
                    revision: 1
                }
            ))
            .await,
        Err(PersistenceError::RequestConflict)
    ));
    let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    let facts: Vec<(i64, Option<String>)> = db
        .prepare(
            "SELECT change_kind, text FROM steering_mutation_requests ORDER BY accepted_sequence",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        facts,
        vec![
            (1, Some("Queued".into())),
            (2, Some("Edited".into())),
            (5, None),
            (4, None),
        ]
    );
    drop(db);
    drop(store);
    let store = SessionStore::open_for_test(root.path()).unwrap();
    assert_eq!(store.mutate_steering(enqueue).await.unwrap(), receipt);
    store
        .mutate_steering(mutation(
            9,
            4,
            SteeringChange::Remove {
                item_id: receipt.item_id.unwrap(),
                revision: 2,
            },
        ))
        .await
        .unwrap();
    assert!(matches!(
        store
            .mutate_steering(mutation(10, 5, SteeringChange::Resume { run_id: run.id }))
            .await,
        Err(PersistenceError::RequestConflict)
    ));
}

fn steering_projection_snapshot(db: &Connection) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    [
        "SELECT * FROM steering_queues ORDER BY session_id",
        "SELECT * FROM steering_pending_messages ORDER BY session_id, slot",
    ]
    .into_iter()
    .map(|sql| {
        let mut statement = db.prepare(sql).unwrap();
        let columns = statement.column_count();
        statement
            .query_map([], |row| (0..columns).map(|index| row.get(index)).collect())
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    })
    .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn steering_startup_repairs_pending_projection_but_rejects_item_history_corruption() {
    use crate::persistence::steering::{SteeringChange, SteeringMutation};
    for corruption in [
        "UPDATE steering_mutation_requests SET operation_fingerprint = zeroblob(32)",
        "UPDATE steering_mutation_requests SET text = 'Changed historical text' WHERE queue_revision = 2",
        "UPDATE steering_pending_messages SET text = 'Changed'",
        "UPDATE steering_pending_messages SET revision = revision + 1",
        "UPDATE steering_pending_messages SET enqueue_sequence = enqueue_sequence + 1000",
        "DELETE FROM steering_pending_messages",
        "DELETE FROM steering_pending_messages; DELETE FROM steering_queues",
        "INSERT INTO steering_pending_messages
            (item_id, session_id, slot, enqueue_sequence, revision, text, actor, created_at_milliseconds)
            SELECT randomblob(16), session_id, 2, enqueue_sequence + 1000, 1, 'Unattributed', 1,
                created_at_milliseconds FROM steering_pending_messages",
        "UPDATE steering_queues SET paused = 0",
        "UPDATE steering_mutation_requests SET item_revision = 4 WHERE change_kind = 2 AND queue_revision = 2",
        "UPDATE steering_mutation_requests SET change_kind = 3, text = NULL, item_revision = 1 WHERE queue_revision = 2",
        "UPDATE steering_mutation_requests SET change_kind = 1, item_revision = 1,
            target_run_id = (SELECT target_run_id FROM steering_queues) WHERE queue_revision = 2",
        "UPDATE steering_mutation_requests SET change_kind = 2, item_revision = 2,
            target_run_id = NULL WHERE queue_revision = 1",
    ] {
        let root = TestRoot::new("steering-corruption");
        let store = SessionStore::open_for_test(root.path()).unwrap();
        configure_credential(&store).await;
        let session = store
            .create_session(MutationRequestId::from_bytes([0xc1; 16]), None)
            .await
            .unwrap();
        let run = store
            .accept_session_input(
                MutationRequestId::from_bytes([0xc2; 16]),
                session.id,
                "Initial".into(),
                model_selection(),
            )
            .await
            .unwrap()
            .run;
        let enqueued = store
            .mutate_steering(SteeringMutation {
                request_id: MutationRequestId::from_bytes([0xc3; 16]),
                session_id: session.id,
                expected_revision: 0,
                change: SteeringChange::Enqueue {
                    run_id: run.id,
                    text: "Queued".into(),
                },
            })
            .await
            .unwrap();
        for revision in 1..=2 {
            store
                .mutate_steering(SteeringMutation {
                    request_id: MutationRequestId::from_bytes([0xc3 + revision as u8; 16]),
                    session_id: session.id,
                    expected_revision: revision,
                    change: SteeringChange::Edit {
                        item_id: enqueued.item_id.unwrap(),
                        revision,
                        text: format!("Edited {revision}"),
                    },
                })
                .await
                .unwrap();
        }
        store
            .mutate_steering(SteeringMutation {
                request_id: MutationRequestId::from_bytes([0xc6; 16]),
                session_id: session.id,
                expected_revision: 3,
                change: SteeringChange::Pause,
            })
            .await
            .unwrap();
        drop(store);
        // Verify the intact history before corrupting an isolated database.
        drop(SessionStore::open_for_test(root.path()).unwrap());
        let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        let expected = steering_projection_snapshot(&db);
        db.execute_batch(corruption).unwrap();
        drop(db);
        if corruption.contains("steering_mutation_requests") {
            assert!(
                matches!(
                    SessionStore::open_for_test(root.path()),
                    Err(PersistenceError::InvalidState { .. })
                ),
                "{corruption}"
            );
        } else {
            for _ in 0..2 {
                drop(SessionStore::open_for_test(root.path()).unwrap());
                let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
                assert_eq!(steering_projection_snapshot(&db), expected, "{corruption}");
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn steering_initial_history_requires_enqueue_even_with_matching_fingerprint() {
    use crate::persistence::steering::{SteeringChange, SteeringMutation, fingerprint};
    for resume in [false, true] {
        let root = TestRoot::new("steering-initial-history");
        let store = SessionStore::open_for_test(root.path()).unwrap();
        configure_credential(&store).await;
        let session = store
            .create_session(MutationRequestId::from_bytes([0xd1; 16]), None)
            .await
            .unwrap();
        let run = store
            .accept_session_input(
                MutationRequestId::from_bytes([0xd2; 16]),
                session.id,
                "Initial".into(),
                model_selection(),
            )
            .await
            .unwrap()
            .run;
        store
            .mutate_steering(SteeringMutation {
                request_id: MutationRequestId::from_bytes([0xd3; 16]),
                session_id: session.id,
                expected_revision: 0,
                change: SteeringChange::Enqueue {
                    run_id: run.id,
                    text: "Queued".into(),
                },
            })
            .await
            .unwrap();
        drop(store);
        let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        let change = if resume {
            SteeringChange::Resume { run_id: run.id }
        } else {
            SteeringChange::Pause
        };
        db.execute("DELETE FROM steering_pending_messages", [])
            .unwrap();
        db.execute(
            "UPDATE steering_mutation_requests SET change_kind = ?1,
                target_run_id = ?2, item_id = NULL, item_revision = NULL, text = NULL,
                operation_fingerprint = ?3",
            rusqlite::params![
                if resume { 5 } else { 4 },
                resume.then_some(run.id.as_bytes()),
                fingerprint(session.id, 0, &change),
            ],
        )
        .unwrap();
        db.execute(
            "UPDATE steering_queues SET paused = ?1",
            [i64::from(!resume)],
        )
        .unwrap();
        drop(db);
        assert!(matches!(
            SessionStore::open_for_test(root.path()),
            Err(PersistenceError::InvalidState {
                reason: "steering lifecycle history conflicts with its source or queue"
            })
        ));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn steering_startup_repairs_queue_projection_but_rejects_lifecycle_corruption() {
    use crate::persistence::steering::{SteeringChange, SteeringMutation};
    for scenario in [
        "initial",
        "resume",
        "target",
        "lifecycle-source",
        "lifecycle-missing",
        "lifecycle-state",
        "lifecycle-revision",
        "lifecycle-time",
        "lifecycle-missing-rewound",
        "cancellation-missing-rewound",
        "archive-missing-rewound",
        "resume-after-terminal",
        "resume-after-cancellation",
    ] {
        let root = TestRoot::new("steering-queue-corruption");
        let store = SessionStore::open_for_test(root.path()).unwrap();
        configure_credential(&store).await;
        let session = store
            .create_session(MutationRequestId::from_bytes([0xd1; 16]), None)
            .await
            .unwrap();
        let run = store
            .accept_session_input(
                MutationRequestId::from_bytes([0xd2; 16]),
                session.id,
                "Initial".into(),
                model_selection(),
            )
            .await
            .unwrap()
            .run;
        store
            .mutate_steering(SteeringMutation {
                request_id: MutationRequestId::from_bytes([0xd3; 16]),
                session_id: session.id,
                expected_revision: 0,
                change: SteeringChange::Enqueue {
                    run_id: run.id,
                    text: "Queued".into(),
                },
            })
            .await
            .unwrap();
        let corruption = match scenario {
            "initial" => "UPDATE steering_queues SET paused = 0",
            "resume" => {
                store
                    .mutate_steering(SteeringMutation {
                        request_id: MutationRequestId::from_bytes([0xd4; 16]),
                        session_id: session.id,
                        expected_revision: 1,
                        change: SteeringChange::Resume { run_id: run.id },
                    })
                    .await
                    .unwrap();
                "UPDATE steering_queues SET paused = 1"
            }
            "lifecycle-source"
            | "lifecycle-missing"
            | "lifecycle-state"
            | "lifecycle-revision"
            | "lifecycle-time"
            | "lifecycle-missing-rewound"
            | "cancellation-missing-rewound"
            | "archive-missing-rewound"
            | "resume-after-terminal"
            | "resume-after-cancellation" => {
                store
                    .mutate_steering(SteeringMutation {
                        request_id: MutationRequestId::from_bytes([0xd4; 16]),
                        session_id: session.id,
                        expected_revision: 1,
                        change: SteeringChange::Resume { run_id: run.id },
                    })
                    .await
                    .unwrap();
                match scenario {
                    "archive-missing-rewound" => {
                        store
                            .prepare_session_archive(
                                MutationRequestId::from_bytes([0xd6; 16]),
                                session.id,
                                true,
                            )
                            .await
                            .unwrap();
                        assert!(matches!(
                            store
                                .mutate_steering(SteeringMutation {
                                    request_id: MutationRequestId::from_bytes([0xd7; 16]),
                                    session_id: session.id,
                                    expected_revision: 3,
                                    change: SteeringChange::Resume { run_id: run.id },
                                })
                                .await,
                            Err(PersistenceError::SessionArchived)
                        ));
                        store.finish_run_stopped(run.id, None).await.unwrap();
                        store
                            .complete_session_archive(MutationRequestId::from_bytes([0xd6; 16]))
                            .await
                            .unwrap();
                    }
                    "cancellation-missing-rewound" | "resume-after-cancellation" => {
                        store
                            .cancel_run(
                                MutationRequestId::from_bytes([0xd6; 16]),
                                session.id,
                                run.id,
                            )
                            .await
                            .unwrap();
                    }
                    _ => {
                        store.finish_run_stopped(run.id, None).await.unwrap();
                    }
                }
                match scenario {
                    "resume-after-terminal" | "resume-after-cancellation" => {
                        "DELETE FROM steering_lifecycle_facts;
                         UPDATE steering_queues SET revision = 2, paused = 0;
                         UPDATE steering_mutation_requests SET accepted_sequence = accepted_sequence + 1000
                         WHERE change_kind = 5;
                         UPDATE mutation_requests SET accepted_sequence = accepted_sequence + 1000
                         WHERE request_id IN (SELECT request_id FROM steering_mutation_requests WHERE change_kind = 5)"
                    }
                    "lifecycle-source" => "UPDATE steering_lifecycle_facts SET source_sequence = 1",
                    "lifecycle-missing" => "DELETE FROM steering_lifecycle_facts",
                    "lifecycle-missing-rewound" | "cancellation-missing-rewound" | "archive-missing-rewound" => {
                        "DELETE FROM steering_lifecycle_facts;
                         UPDATE steering_queues SET revision = 2, paused = 0"
                    }
                    "lifecycle-state" => "UPDATE steering_queues SET paused = 0",
                    "lifecycle-revision" => {
                        "UPDATE steering_lifecycle_facts SET queue_revision = queue_revision + 1"
                    }
                    _ => {
                        "UPDATE steering_lifecycle_facts SET created_at_milliseconds = created_at_milliseconds + 1"
                    }
                }
            }
            "target" => {
                store.finish_run_stopped(run.id, None).await.unwrap();
                store
                    .accept_session_input(
                        MutationRequestId::from_bytes([0xd5; 16]),
                        session.id,
                        "Next".into(),
                        model_selection(),
                    )
                    .await
                    .unwrap();
                "UPDATE steering_queues SET target_run_id =
                    (SELECT run_id FROM runs ORDER BY accepted_sequence DESC LIMIT 1)"
            }
            _ => unreachable!(),
        };
        drop(store);
        let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        let mut expected = steering_projection_snapshot(&db);
        if scenario == "resume" {
            // Recovery pauses the reconstructed active queue exactly once.
            expected[0][0][2] = rusqlite::types::Value::Integer(3);
            expected[0][0][3] = rusqlite::types::Value::Integer(1);
        }
        db.execute_batch(corruption).unwrap();
        drop(db);
        if matches!(
            scenario,
            "initial" | "resume" | "target" | "lifecycle-state"
        ) {
            for _ in 0..2 {
                drop(SessionStore::open_for_test(root.path()).unwrap());
                let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
                assert_eq!(steering_projection_snapshot(&db), expected, "{scenario}");
            }
        } else {
            assert!(
                matches!(
                    SessionStore::open_for_test(root.path()),
                    Err(PersistenceError::InvalidState {
                        reason: "steering lifecycle history conflicts with its source or queue"
                    })
                ),
                "{scenario}"
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn steering_startup_rejects_transient_overcapacity_with_valid_final_projection() {
    use crate::persistence::steering::{SteeringChange, SteeringMutation, fingerprint};
    let root = TestRoot::new("steering-historical-capacity");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    configure_credential(&store).await;
    let session = store
        .create_session(MutationRequestId::from_bytes([0xc1; 16]), None)
        .await
        .unwrap();
    let run = store
        .accept_session_input(
            MutationRequestId::from_bytes([0xc2; 16]),
            session.id,
            "Initial".into(),
            model_selection(),
        )
        .await
        .unwrap()
        .run;
    let mut first = None;
    for revision in 0..16 {
        let receipt = store
            .mutate_steering(SteeringMutation {
                request_id: MutationRequestId::from_bytes([10 + revision as u8; 16]),
                session_id: session.id,
                expected_revision: revision,
                change: SteeringChange::Enqueue {
                    run_id: run.id,
                    text: "Queued".into(),
                },
            })
            .await
            .unwrap();
        first.get_or_insert(receipt);
    }
    let remove = SteeringChange::Remove {
        item_id: first.unwrap().item_id.unwrap(),
        revision: 1,
    };
    let enqueue = SteeringChange::Enqueue {
        run_id: run.id,
        text: "Replacement".into(),
    };
    for (expected_revision, change) in [(16, remove.clone()), (17, enqueue.clone())] {
        store
            .mutate_steering(SteeringMutation {
                request_id: MutationRequestId::from_bytes([30 + expected_revision as u8; 16]),
                session_id: session.id,
                expected_revision,
                change,
            })
            .await
            .unwrap();
    }
    drop(store);
    drop(SessionStore::open_for_test(root.path()).unwrap());
    let mut db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    let tx = db.transaction().unwrap();
    // Swap payloads, retaining registry bindings and a consistent final projection.
    tx.execute_batch(
        "CREATE TEMP TABLE swapped AS SELECT * FROM steering_mutation_requests
            WHERE queue_revision IN (17, 18);
         UPDATE steering_mutation_requests AS request SET
            (change_kind, target_run_id, item_id, item_revision, text) =
            (SELECT change_kind, target_run_id, item_id, item_revision, text FROM swapped
             WHERE queue_revision = 35 - request.queue_revision)
         WHERE queue_revision IN (17, 18);
         UPDATE steering_pending_messages SET
            enqueue_sequence = (SELECT accepted_sequence FROM steering_mutation_requests WHERE queue_revision = 17),
            created_at_milliseconds = (SELECT accepted_at_milliseconds FROM steering_mutation_requests WHERE queue_revision = 17)
         WHERE item_id = (SELECT item_id FROM steering_mutation_requests WHERE queue_revision = 17);",
    )
    .unwrap();
    for (revision, change) in [(17, enqueue), (18, remove)] {
        tx.execute(
            "UPDATE steering_mutation_requests SET operation_fingerprint = ?1 WHERE queue_revision = ?2",
            rusqlite::params![fingerprint(session.id, revision - 1, &change), i64::try_from(revision).unwrap()],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM steering_pending_messages",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        16
    );
    drop(db);
    assert!(matches!(
        SessionStore::open_for_test(root.path()),
        Err(PersistenceError::InvalidState {
            reason: "steering history exceeds queue capacity"
        })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn steering_capacity_fifo_stale_targets_and_deleted_retries() {
    use crate::persistence::steering::{SteeringChange, SteeringMutation};
    let root = TestRoot::new("steering-capacity");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    configure_credential(&store).await;
    let session = store
        .create_session(MutationRequestId::from_bytes([0xc1; 16]), None)
        .await
        .unwrap();
    let run = store
        .accept_session_input(
            MutationRequestId::from_bytes([0xc2; 16]),
            session.id,
            "Initial".into(),
            model_selection(),
        )
        .await
        .unwrap()
        .run;
    let mutation = |id, expected_revision, change| SteeringMutation {
        request_id: MutationRequestId::from_bytes([id; 16]),
        session_id: session.id,
        expected_revision,
        change,
    };
    let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    let mut first = None;
    for revision in 0..16 {
        let receipt = store
            .mutate_steering(mutation(
                10 + revision as u8,
                revision,
                SteeringChange::Enqueue {
                    run_id: run.id,
                    text: "é".repeat(32768),
                },
            ))
            .await
            .unwrap();
        first.get_or_insert(receipt);
    }
    let first = first.unwrap();
    let full = mutation(
        30,
        16,
        SteeringChange::Enqueue {
            run_id: run.id,
            text: "Overflow".into(),
        },
    );
    assert!(matches!(
        store.mutate_steering(full).await,
        Err(PersistenceError::InvalidInput { .. })
    ));
    assert_eq!(
        db.query_row(
            "SELECT SUM(length(CAST(text AS BLOB))) FROM steering_pending_messages",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1_048_576
    );
    store
        .mutate_steering(mutation(
            31,
            16,
            SteeringChange::Remove {
                item_id: first.item_id.unwrap(),
                revision: 1,
            },
        ))
        .await
        .unwrap();
    let last = store
        .mutate_steering(mutation(
            32,
            17,
            SteeringChange::Enqueue {
                run_id: run.id,
                text: "Last".into(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(
        db.query_row(
            "SELECT item_id FROM steering_pending_messages ORDER BY enqueue_sequence DESC LIMIT 1",
            [],
            |r| r.get::<_, [u8; 16]>(0)
        )
        .unwrap(),
        last.item_id.unwrap()
    );
    let stale = mutation(
        33,
        18,
        SteeringChange::Enqueue {
            run_id: crate::persistence::RunId::from_bytes([0xff; 16]),
            text: "Wrong target".into(),
        },
    );
    assert!(matches!(
        store.mutate_steering(stale).await,
        Err(PersistenceError::RequestConflict)
    ));
    store.finish_run_stopped(run.id, None).await.unwrap();
    let terminal = mutation(
        34,
        18,
        SteeringChange::Enqueue {
            run_id: run.id,
            text: "Terminal".into(),
        },
    );
    assert!(matches!(
        store.mutate_steering(terminal).await,
        Err(PersistenceError::RequestConflict)
    ));
    let next_run = store
        .accept_session_input(
            MutationRequestId::from_bytes([0xc5; 16]),
            session.id,
            "Next run".into(),
            model_selection(),
        )
        .await
        .unwrap()
        .run;
    assert!(matches!(
        store
            .mutate_steering(mutation(
                35,
                18,
                SteeringChange::Enqueue {
                    run_id: next_run.id,
                    text: "Must not retarget".into(),
                },
            ))
            .await,
        Err(PersistenceError::RequestConflict)
    ));
    assert_eq!(
        db.query_row(
            "SELECT target_run_id, revision, paused FROM steering_queues",
            [],
            |row| Ok((
                row.get::<_, [u8; 16]>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?
            )),
        )
        .unwrap(),
        (*run.id.as_bytes(), 18, 1)
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM mutation_requests WHERE request_id = ?1",
            [[35_u8; 16]],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );
    store
        .mutate_steering(mutation(
            36,
            18,
            SteeringChange::Resume {
                run_id: next_run.id,
            },
        ))
        .await
        .unwrap();
    store.finish_run_stopped(next_run.id, None).await.unwrap();
    store
        .set_session_archived(MutationRequestId::from_bytes([0xc3; 16]), session.id, true)
        .await
        .unwrap();
    store
        .delete_session(MutationRequestId::from_bytes([0xc4; 16]), session.id)
        .await
        .unwrap();
    assert!(matches!(
        store
            .mutate_steering(mutation(
                32,
                17,
                SteeringChange::Enqueue {
                    run_id: run.id,
                    text: "Last".into()
                }
            ))
            .await,
        Err(PersistenceError::RequestConflict)
    ));
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM steering_mutation_requests", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(store);
    SessionStore::open_for_test(root.path()).unwrap();
}
