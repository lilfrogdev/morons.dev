use super::*;

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
