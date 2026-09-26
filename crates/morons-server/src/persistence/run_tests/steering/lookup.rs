use super::*;
use crate::persistence::steering::{SteeringChange, SteeringMutation};

#[tokio::test(flavor = "current_thread")]
async fn steering_lookup_is_read_only_and_preserves_admission_guards() {
    let root = TestRoot::new("steering-lookup");
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
    let mutation = SteeringMutation {
        request_id: MutationRequestId::from_bytes([0xe3; 16]),
        session_id: session.id,
        expected_revision: 0,
        change: SteeringChange::Enqueue {
            run_id: run.id,
            text: "Queued".into(),
        },
    };
    let empty = store.steering_snapshot(session.id).await.unwrap();
    assert_eq!(
        store
            .lookup_steering_mutation(mutation.clone())
            .await
            .unwrap(),
        None
    );
    assert_eq!(store.steering_snapshot(session.id).await.unwrap(), empty);
    assert!(
        store
            .steering_replay(empty.cursor, 1)
            .await
            .unwrap()
            .notices
            .is_empty()
    );

    let competing = SteeringMutation {
        request_id: MutationRequestId::from_bytes([0xe4; 16]),
        ..mutation.clone()
    };
    let receipt = store.mutate_steering(competing.clone()).await.unwrap();
    assert!(matches!(
        store.mutate_steering(mutation.clone()).await,
        Err(PersistenceError::RequestConflict)
    ));
    assert_eq!(
        store
            .lookup_steering_mutation(mutation.clone())
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .lookup_steering_mutation(competing.clone())
            .await
            .unwrap(),
        Some(receipt.clone())
    );
    drop(store);

    let store = SessionStore::open_for_test(root.path()).unwrap();
    let before = store.steering_snapshot(session.id).await.unwrap();
    assert_eq!(
        store
            .lookup_steering_mutation(competing.clone())
            .await
            .unwrap(),
        Some(receipt.clone())
    );
    assert_eq!(
        store.mutate_steering(competing.clone()).await.unwrap(),
        receipt
    );
    let mut conflicts = Vec::new();
    let mut changed = competing.clone();
    changed.expected_revision += 1;
    conflicts.push(changed);
    let mut changed = competing.clone();
    changed.session_id = crate::persistence::SessionId::from_bytes([0xee; 16]);
    conflicts.push(changed);
    let mut changed = competing.clone();
    changed.change = SteeringChange::Enqueue {
        run_id: run.id,
        text: "Changed".into(),
    };
    conflicts.push(changed);
    let mut changed = competing.clone();
    changed.change = SteeringChange::Enqueue {
        run_id: crate::persistence::RunId::from_bytes([0xef; 16]),
        text: "Queued".into(),
    };
    conflicts.push(changed);
    let mut changed = competing.clone();
    changed.change = SteeringChange::Pause;
    conflicts.push(changed);
    let mut changed = competing.clone();
    changed.request_id = MutationRequestId::from_bytes([0xe1; 16]);
    conflicts.push(changed);
    for conflict in conflicts {
        assert!(matches!(
            store.lookup_steering_mutation(conflict).await,
            Err(PersistenceError::RequestConflict)
        ));
    }
    let mut zero = competing;
    zero.request_id = MutationRequestId::from_bytes([0; 16]);
    assert!(matches!(
        store.lookup_steering_mutation(zero).await,
        Err(PersistenceError::InvalidInput { .. })
    ));
    assert_eq!(store.steering_snapshot(session.id).await.unwrap(), before);
    let replay = store.steering_replay(before.cursor, 1).await.unwrap();
    assert!(replay.notices.is_empty());
    assert_eq!(replay.high_water, before.cursor);
}
