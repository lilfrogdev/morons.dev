use super::*;
use crate::persistence::steering::{SteeringChange, SteeringMutation};
use crate::skills::SkillDiscovery;

#[tokio::test(flavor = "current_thread")]
async fn steering_skills_survive_retry_restart_and_reject_corruption() {
    let root = TestRoot::new("steering-skills");
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
    let mutation = SteeringMutation {
        request_id: MutationRequestId::from_bytes([0xd3; 16]),
        session_id: session.id,
        expected_revision: 0,
        change: SteeringChange::Enqueue {
            run_id: run.id,
            text: "@skill-creator".into(),
        },
    };
    let skills = SkillDiscovery::for_test(vec![]).context(root.path(), "@skill-creator");
    assert!(skills.skills.iter().any(|skill| skill.active));
    let receipt = store
        .mutate_steering_with_skills(mutation.clone(), Some(skills))
        .await
        .unwrap();
    let db_path = root.path().join("data/sessions.sqlite3");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let snapshot = || {
        db.query_row(
            "SELECT skill_context, skill_context_digest FROM steering_mutation_requests
             WHERE request_id = ?1",
            [mutation.request_id.as_bytes()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .unwrap()
    };
    let before = snapshot();
    assert_eq!(
        store
            .mutate_steering_with_skills(mutation.clone(), Some(Default::default()))
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(snapshot(), before);
    drop(store);
    let store = SessionStore::open_for_test(root.path()).unwrap();
    assert_eq!(
        store
            .lookup_steering_mutation(mutation.clone())
            .await
            .unwrap(),
        Some(receipt.clone())
    );
    assert_eq!(snapshot(), before);
    store
        .mutate_steering_with_skills(
            SteeringMutation {
                request_id: MutationRequestId::from_bytes([0xd4; 16]),
                session_id: session.id,
                expected_revision: receipt.queue_revision,
                change: SteeringChange::Edit {
                    item_id: receipt.item_id.unwrap(),
                    revision: receipt.item_revision.unwrap(),
                    text: "Revised without skills".into(),
                },
            },
            Some(Default::default()),
        )
        .await
        .unwrap();
    assert_eq!(snapshot(), before);
    drop(store);
    let store = SessionStore::open_for_test(root.path()).unwrap();
    drop(store);
    // Superseded text revisions remain subject to startup integrity validation.
    db.execute(
        "UPDATE steering_mutation_requests SET skill_context_digest = zeroblob(32)
         WHERE request_id = ?1",
        [mutation.request_id.as_bytes()],
    )
    .unwrap();
    assert!(SessionStore::open_for_test(root.path()).is_err());
}
