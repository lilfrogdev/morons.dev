use super::*;
use crate::persistence::steering::{SteeringChange, SteeringMutation};
use rusqlite::params;

async fn populated_profile() -> TestRoot {
    let root = TestRoot::new("schema46-steering-compatibility");
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
                text: "Keep literal @skill".into(),
            },
        })
        .await
        .unwrap();
    drop(store);
    root
}

fn fingerprint(db: &Connection) -> Vec<u8> {
    db.query_row(
        "SELECT operation_fingerprint FROM steering_mutation_requests",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn schema46_migrates_legacy_steering_without_rewriting_fingerprints() {
    let root = populated_profile().await;
    let path = root.path().join("data/sessions.sqlite3");
    let db = Connection::open(&path).unwrap();
    let before = fingerprint(&db);
    // Reconstruct the schema45 table by removing only the two additive columns.
    db.execute_batch(
        "ALTER TABLE steering_mutation_requests DROP COLUMN skill_context_digest;
         ALTER TABLE steering_mutation_requests DROP COLUMN skill_context;
         PRAGMA user_version = 45;",
    )
    .unwrap();
    drop(db);

    drop(SessionStore::open_for_test(root.path()).unwrap());
    let db = Connection::open(&path).unwrap();
    assert_eq!(fingerprint(&db), before);
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        crate::persistence::database::SCHEMA_VERSION
    );
    assert_eq!(db.query_row(
        "SELECT count(*) FROM steering_mutation_requests WHERE skill_context IS NULL AND skill_context_digest IS NULL",
        [], |row| row.get::<_, i64>(0),
    ).unwrap(), 1);
    drop(db);

    let backup = Connection::open(
        root.path()
            .join("backups/sessions-before-schema-v45.sqlite3"),
    )
    .unwrap();
    assert_eq!(
        backup
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        45
    );
    assert_eq!(fingerprint(&backup), before);
    drop(backup);
    // Existing schema46 profiles with legacy records also reopen normally.
    drop(SessionStore::open_for_test(root.path()).unwrap());
    assert_eq!(fingerprint(&Connection::open(path).unwrap()), before);
}

#[tokio::test(flavor = "current_thread")]
async fn schema46_rejects_populated_reserved_columns_without_erasing_them() {
    for (context, digest) in [
        (Some("prototype snapshot"), Some(vec![7_u8; 32])),
        (Some("prototype snapshot"), None),
        (None, Some(vec![7_u8; 32])),
    ] {
        let root = populated_profile().await;
        let path = root.path().join("data/sessions.sqlite3");
        let db = Connection::open(&path).unwrap();
        // Include independently populated columns even if constraints were bypassed.
        db.execute_batch("PRAGMA ignore_check_constraints = ON;")
            .unwrap();
        db.execute(
            "UPDATE steering_mutation_requests SET skill_context = ?1, skill_context_digest = ?2",
            params![context, digest],
        )
        .unwrap();
        let before = fingerprint(&db);
        drop(db);
        assert!(matches!(
            SessionStore::open_for_test(root.path()),
            Err(PersistenceError::InvalidState {
                reason: "stored steering skill context is invalid"
            })
        ));
        let db = Connection::open(path).unwrap();
        let retained: (Option<String>, Option<Vec<u8>>) = db
            .query_row(
                "SELECT skill_context, skill_context_digest FROM steering_mutation_requests",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(retained, (context.map(str::to_owned), digest));
        assert_eq!(fingerprint(&db), before);
    }
}
