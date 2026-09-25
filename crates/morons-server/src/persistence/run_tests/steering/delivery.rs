use rusqlite::params;

use super::*;
use crate::persistence::steering::{
    SteeringChange, SteeringCursor, SteeringMutation, SteeringPage,
};

async fn assert_delivery_replay_pages(
    store: &SessionStore,
    start: SteeringCursor,
    expected: &SteeringPage,
) {
    let mut cursor = start;
    for notice in &expected.notices {
        let page = store.steering_replay(cursor, 1).await.unwrap();
        assert_eq!(page.high_water, expected.high_water);
        assert_eq!(page.notices, std::slice::from_ref(notice));
        assert!(notice.cursor.sequence > cursor.sequence);
        cursor = notice.cursor;
    }
    assert_eq!(cursor, expected.high_water);
    let exhausted = store.steering_replay(cursor, 1).await.unwrap();
    assert!(exhausted.notices.is_empty());
    assert_eq!(exhausted.high_water, expected.high_water);
}

#[tokio::test(flavor = "current_thread")]
async fn steering_delivery_schema_migrates_populated_v44_without_consumption() {
    let root = TestRoot::new("steering-delivery-migration");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    configure_credential(&store).await;
    let session = store
        .create_session(MutationRequestId::from_bytes([0xf1; 16]), None)
        .await
        .unwrap();
    let run = store
        .accept_session_input(
            MutationRequestId::from_bytes([0xf2; 16]),
            session.id,
            "Initial".into(),
            model_selection(),
        )
        .await
        .unwrap()
        .run;
    store
        .mutate_steering(SteeringMutation {
            request_id: MutationRequestId::from_bytes([0xf3; 16]),
            session_id: session.id,
            expected_revision: 0,
            change: SteeringChange::Enqueue {
                run_id: run.id,
                text: "Pending".into(),
            },
        })
        .await
        .unwrap();
    let before = store.steering_snapshot(session.id).await.unwrap();
    drop(store);
    let database_path = root.path().join("data/sessions.sqlite3");
    let db = Connection::open(&database_path).unwrap();
    db.execute_batch(
        "BEGIN IMMEDIATE;
         ALTER TABLE steering_mutation_requests DROP COLUMN skill_context_digest;
         ALTER TABLE steering_mutation_requests DROP COLUMN skill_context;
         DROP TABLE steering_delivery_facts;
         DROP INDEX session_entries_user_by_run;
         CREATE UNIQUE INDEX session_entries_user_by_run
             ON session_entries (run_id) WHERE entry_kind = 1;
         PRAGMA user_version = 44;
         COMMIT;",
    )
    .unwrap();
    drop(db);
    let store = SessionStore::open_for_test(root.path()).unwrap();
    assert_eq!(store.steering_snapshot(session.id).await.unwrap(), before);
    drop(store);
    let db = Connection::open(&database_path).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        crate::persistence::database::SCHEMA_VERSION
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM steering_delivery_facts", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM session_entries WHERE entry_kind = 1",
            [],
            |row| { row.get::<_, i64>(0) }
        )
        .unwrap(),
        1
    );
}

#[tokio::test(flavor = "current_thread")]
async fn steering_deliveries_rebuild_and_provenance_survive_restart() {
    let root = TestRoot::new("steering-delivery-provenance");
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
    store.activate_run(run.id).await.unwrap();
    let receipt = store
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
    let second = store
        .mutate_steering(SteeringMutation {
            request_id: MutationRequestId::from_bytes([0xe7; 16]),
            session_id: session.id,
            expected_revision: 1,
            change: SteeringChange::Enqueue {
                run_id: run.id,
                text: "Second".into(),
            },
        })
        .await
        .unwrap();
    store
        .mutate_steering(SteeringMutation {
            request_id: MutationRequestId::from_bytes([0xe4; 16]),
            session_id: session.id,
            expected_revision: 2,
            change: SteeringChange::Resume { run_id: run.id },
        })
        .await
        .unwrap();
    let before = store.steering_snapshot(session.id).await.unwrap();
    drop(store);
    let mut db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    let tx = db.transaction().unwrap();
    let sequence: i64 = tx
        .query_row("SELECT next_value FROM logical_sequences", [], |r| r.get(0))
        .unwrap();
    // Construct canonical delivery without enabling a production consumption path.
    tx.execute(
        "INSERT INTO session_entries
        (fact_id, fact_sequence, session_id, entry_sequence, message_id, run_id, entry_kind,
         actor_kind, text, refusal, created_at_milliseconds, delivery_event_id)
        VALUES (randomblob(16), ?1, ?2, 2, ?3, ?4, 1, 1, 'Queued', 0, 100, randomblob(16))",
        params![
            sequence,
            session.id.as_bytes(),
            [0xd1_u8; 16],
            run.id.as_bytes()
        ],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO steering_delivery_facts VALUES (?1, ?2, ?3, ?4, 1, ?5, 4, ?6, 1, 100)",
        params![
            sequence + 1,
            session.id.as_bytes(),
            run.id.as_bytes(),
            receipt.item_id.unwrap(),
            i64::try_from(receipt.sequence).unwrap(),
            [0xd1_u8; 16]
        ],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO session_entries
        (fact_id, fact_sequence, session_id, entry_sequence, message_id, run_id, entry_kind,
         actor_kind, text, refusal, created_at_milliseconds, delivery_event_id)
        VALUES (randomblob(16), ?1, ?2, 3, ?3, ?4, 1, 1, 'Second', 0, 101, randomblob(16))",
        params![
            sequence + 2,
            session.id.as_bytes(),
            [0xd2_u8; 16],
            run.id.as_bytes()
        ],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO steering_delivery_facts VALUES (?1, ?2, ?3, ?4, 1, ?5, 5, ?6, 2, 101)",
        params![
            sequence + 3,
            session.id.as_bytes(),
            run.id.as_bytes(),
            second.item_id.unwrap(),
            i64::try_from(second.sequence).unwrap(),
            [0xd2_u8; 16]
        ],
    )
    .unwrap();
    tx.execute(
        "UPDATE logical_sequences SET next_value = ?1",
        [sequence + 4],
    )
    .unwrap();
    tx.commit().unwrap();
    drop(db);
    let store = SessionStore::open_for_test(root.path()).unwrap();
    let after = store.steering_snapshot(session.id).await.unwrap();
    assert!(after.items.is_empty());
    assert!(after.paused);
    assert_eq!(after.revision, 6);
    let replay = store.steering_replay(before.cursor, 128).await.unwrap();
    assert_eq!(replay.notices.len(), 3);
    assert_eq!(replay.notices[0].revision, 4);
    assert_eq!(replay.notices[1].revision, 5);
    assert_eq!(replay.notices[2].revision, 6);
    assert_eq!(replay.high_water, after.cursor);
    assert_delivery_replay_pages(&store, before.cursor, &replay).await;
    drop(store);
    let database_path = root.path().join("data/sessions.sqlite3");
    let canonical = std::fs::read(&database_path).unwrap();
    for damage in [
        "INSERT INTO steering_pending_messages
         (item_id, session_id, slot, enqueue_sequence, revision, text, actor, created_at_milliseconds)
         SELECT item_id, session_id, ROW_NUMBER() OVER (ORDER BY accepted_sequence), accepted_sequence, item_revision, text, actor,
                accepted_at_milliseconds
         FROM steering_mutation_requests WHERE change_kind = 1",
        "DELETE FROM steering_pending_messages; DELETE FROM steering_queues",
        "UPDATE steering_queues SET revision = 99, paused = 0",
    ] {
        let db = Connection::open(&database_path).unwrap();
        db.execute_batch(damage).unwrap();
        drop(db);
        let store = SessionStore::open_for_test(root.path()).unwrap();
        assert_eq!(
            store.steering_snapshot(session.id).await.unwrap(),
            after,
            "incorrect reconstruction after {damage}"
        );
        let rebuilt_replay = store.steering_replay(before.cursor, 128).await.unwrap();
        assert_eq!(rebuilt_replay.high_water, replay.high_water);
        assert_eq!(
            rebuilt_replay.notices, replay.notices,
            "reconstruction changed replay after {damage}"
        );
        assert_delivery_replay_pages(&store, before.cursor, &replay).await;
        drop(store);
        std::fs::write(&database_path, &canonical).unwrap();
    }
    for corruption in [
        "DELETE FROM steering_delivery_facts",
        "DELETE FROM steering_delivery_facts WHERE source_entry_high_water = 1",
        "UPDATE steering_delivery_facts SET item_revision = 2",
        "UPDATE steering_delivery_facts SET enqueue_sequence = 1",
        "UPDATE steering_delivery_facts SET source_entry_high_water = 2",
        "UPDATE steering_delivery_facts SET queue_revision = 9",
        "UPDATE steering_delivery_facts SET created_at_milliseconds = 101",
        "UPDATE steering_delivery_facts SET fact_sequence = (SELECT fact_sequence FROM run_accepted_facts)",
        "UPDATE steering_delivery_facts SET fact_sequence = (SELECT next_value FROM logical_sequences)",
        "UPDATE steering_mutation_requests SET change_kind = 4, target_run_id = NULL WHERE change_kind = 5",
        "UPDATE session_entries SET text = 'Changed' WHERE entry_sequence = 2",
        "UPDATE steering_delivery_facts SET message_id = (SELECT user_message_id FROM run_accepted_facts)",
    ] {
        let db = Connection::open(&database_path).unwrap();
        let corruption = if corruption.starts_with("UPDATE steering_delivery_facts") {
            format!(
                "{corruption} WHERE fact_sequence = (SELECT MIN(fact_sequence) FROM steering_delivery_facts)"
            )
        } else {
            corruption.to_owned()
        };
        db.execute_batch(&corruption).unwrap();
        drop(db);
        assert!(
            SessionStore::open_for_test(root.path()).is_err(),
            "accepted {corruption}"
        );
        std::fs::write(&database_path, &canonical).unwrap();
    }
    let store = SessionStore::open_for_test(root.path()).unwrap();
    assert_eq!(store.steering_snapshot(session.id).await.unwrap(), after);
    store
        .set_session_archived(MutationRequestId::from_bytes([0xe6; 16]), session.id, true)
        .await
        .unwrap();
    store
        .delete_session(MutationRequestId::from_bytes([0xe5; 16]), session.id)
        .await
        .unwrap();
    drop(store);
    let store = SessionStore::open_for_test(root.path()).unwrap();
    assert!(matches!(
        store.steering_snapshot(session.id).await,
        Err(PersistenceError::SessionNotFound)
    ));
}
