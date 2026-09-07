use super::super::*;
use crate::persistence::{
    DefaultModelSelection, PrepareOperationOutcome, ProviderOperationFailureState, RunFailureKind,
    RunModelSelection, credential_tests::TestRoot,
};
use crate::provider::{OpenCodeService, find_open_code_model, open_code_models};

fn id(value: u8) -> MutationRequestId {
    MutationRequestId::from_bytes([value; 16])
}

#[tokio::test]
async fn policy_is_independent_idempotent_sequence_checked_and_durable() {
    let root = TestRoot::new("policy-durable");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    assert_eq!(
        store.data_use_policy().await.unwrap(),
        DataUsePolicy::default()
    );
    let default = DefaultModelSelection {
        service: RunOpenCodeService::Go,
        model_id: "gpt-5.6-luna".into(),
    };
    store
        .set_default_model(id(1), default.clone())
        .await
        .unwrap();
    let mut previous = 0;
    for (offset, (training, retention)) in
        [(true, false), (true, true), (false, true), (false, false)]
            .into_iter()
            .enumerate()
    {
        let restrictions = DataUseRestrictions {
            block_training_use: training,
            require_zero_retention: retention,
        };
        let request = id(u8::try_from(offset + 2).unwrap());
        let policy = store
            .set_data_use_policy(request, previous, restrictions)
            .await
            .unwrap();
        assert!(policy.sequence > previous);
        assert_eq!(
            store
                .set_data_use_policy(request, previous, restrictions)
                .await
                .unwrap(),
            policy
        );
        assert!(matches!(
            store
                .set_data_use_policy(request, policy.sequence, restrictions)
                .await,
            Err(PersistenceError::RequestConflict)
        ));
        assert!(matches!(
            store
                .set_data_use_policy(id(99), previous, restrictions)
                .await,
            Err(PersistenceError::DataUsePolicyChanged)
        ));
        assert_eq!(store.default_model().await.unwrap(), Some(default.clone()));
        previous = policy.sequence;
    }
    assert!(matches!(
        store
            .set_data_use_policy(id(1), previous, Default::default())
            .await,
        Err(PersistenceError::RequestConflict)
    ));
    let last = store.data_use_policy().await.unwrap();
    drop(store);
    let store = SessionStore::open_for_test(root.path()).unwrap();
    assert_eq!(store.data_use_policy().await.unwrap(), last);
    assert_eq!(store.default_model().await.unwrap(), Some(default));
}

#[tokio::test]
async fn current_policy_enforces_the_reviewed_matrix_for_selection_and_dispatch() {
    let root = TestRoot::new("policy-matrix");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    let mut previous = 0;
    for (offset, (training, retention)) in
        [(false, false), (true, false), (true, true), (false, true)]
            .into_iter()
            .enumerate()
    {
        let restrictions = DataUseRestrictions {
            block_training_use: training,
            require_zero_retention: retention,
        };
        let policy = store
            .set_data_use_policy(
                id(u8::try_from(offset + 1).unwrap()),
                previous,
                restrictions,
            )
            .await
            .unwrap();
        previous = policy.sequence;
        for model in open_code_models() {
            let service = match model.service {
                OpenCodeService::Zen => RunOpenCodeService::Zen,
                OpenCodeService::Go => RunOpenCodeService::Go,
            };
            let admitted = store.admit_model_data_use(service, model.id).await;
            assert_eq!(
                admitted.is_ok(),
                restrictions.permits(model.data_use),
                "{}",
                model.id
            );
            if !restrictions.permits(model.data_use) {
                assert!(matches!(
                    store
                        .set_default_model(
                            id(99),
                            DefaultModelSelection {
                                service,
                                model_id: model.id.into()
                            }
                        )
                        .await,
                    Err(PersistenceError::DataUseRestricted)
                ));
                assert!(matches!(
                    store
                        .set_subagent_model_setting(
                            id(99),
                            crate::persistence::SubagentModelSetting::OpenCode {
                                service,
                                model_id: model.id.into()
                            }
                        )
                        .await,
                    Err(PersistenceError::DataUseRestricted)
                ));
            }
        }
    }
}

#[tokio::test]
async fn acceptance_and_prepared_dispatch_fail_known_without_changing_selected_model() {
    let root = TestRoot::new("policy-run");
    let selected = TestRoot::new("policy-selected");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    store
        .set_open_code_credential(id(1), 0, b"synthetic-policy-key".to_vec())
        .await
        .unwrap();
    let session = store
        .create_session_at(id(2), None, selected.path().to_str().unwrap().to_owned())
        .await
        .unwrap();
    let model = find_open_code_model(OpenCodeService::Go, "gpt-5.6-luna").unwrap();
    let selection = RunModelSelection {
        service: RunOpenCodeService::Go,
        model_id: model.id.into(),
        protocol_revision: model.protocol_revision,
        maximum_input_tokens: model.maximum_input_tokens,
        maximum_output_tokens: model.maximum_output_tokens,
        supports_tool_calls: true,
        supports_image_input: model.capabilities.image_input,
    };
    let accepted = store
        .accept_session_input(id(3), session.id, "test".into(), selection.clone())
        .await
        .unwrap();
    store.activate_run(accepted.run.id).await.unwrap();
    let context = store.load_run_context(accepted.run.id).await.unwrap();
    let PrepareOperationOutcome::Prepared(operation) = store
        .prepare_provider_operation(
            accepted.run.id,
            context.current_entry_high_water,
            context.estimated_input_tokens,
        )
        .await
        .unwrap()
    else {
        panic!("expected prepared operation")
    };
    store
        .set_data_use_policy(
            id(4),
            0,
            DataUseRestrictions {
                block_training_use: false,
                require_zero_retention: true,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .mark_provider_dispatched(accepted.run.id, operation)
            .await,
        Err(PersistenceError::DataUseRestricted)
    ));
    store
        .finish_run_failure(
            accepted.run.id,
            Some(operation),
            RunFailureKind::DataUseRestricted,
            ProviderOperationFailureState::Failed,
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .accept_session_input(id(5), session.id, "blocked".into(), selection)
            .await,
        Err(PersistenceError::DataUseRestricted)
    ));
    let retry = store
        .find_session_input_retry(id(3), session.id, "test", RunOpenCodeService::Go, model.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retry.run.id, accepted.run.id);
    assert_eq!(retry.run.failure, accepted.run.failure);
    assert_eq!(retry.run.state, accepted.run.state);
    let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    let dispatched: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM provider_operation_facts WHERE fact_kind = 2",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(dispatched, 0);
    assert_eq!(
        db.query_row("SELECT failure_kind FROM runs", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        12
    );
    drop(db);
    drop(store);
    assert!(SessionStore::open_for_test(root.path()).is_ok());
}

#[tokio::test]
async fn policy_corruption_and_missing_mutation_evidence_fail_closed() {
    for corruption in [
        "UPDATE data_use_policies SET block_training_use = 0",
        "UPDATE data_use_policies SET expected_sequence = 1",
        "DELETE FROM mutation_requests WHERE operation_kind = 17",
    ] {
        let root = TestRoot::new("policy-corrupt");
        let store = SessionStore::open_for_test(root.path()).unwrap();
        store
            .create_session_at(id(2), None, root.path().to_str().unwrap().into())
            .await
            .unwrap();
        store
            .set_data_use_policy(
                id(1),
                0,
                DataUseRestrictions {
                    block_training_use: true,
                    require_zero_retention: false,
                },
            )
            .await
            .unwrap();
        let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        db.pragma_update(None, "foreign_keys", false).unwrap();
        db.execute_batch(corruption).unwrap();
        drop(db);
        assert!(store.data_use_policy().await.is_err());
        drop(store);
        assert!(SessionStore::open_for_test(root.path()).is_err());
    }
}

#[tokio::test]
async fn populated_schema_28_migration_preserves_history_credentials_and_default_policy() {
    let root = TestRoot::new("policy-migration");
    let selected = TestRoot::new("policy-migration-selected");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    store
        .create_session_at(
            id(1),
            Some("preserved".into()),
            selected.path().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    store
        .set_open_code_credential(id(2), 0, b"synthetic-migration-key".to_vec())
        .await
        .unwrap();
    drop(store);
    let key = std::fs::read(root.path().join("credentials/opencode.state")).unwrap();
    let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    super::restore_schema_28(&db);
    drop(db);
    let store = SessionStore::open_for_test(root.path()).unwrap();
    assert_eq!(
        store.data_use_policy().await.unwrap(),
        DataUsePolicy::default()
    );
    assert_eq!(
        store
            .open_code_credential_status()
            .await
            .unwrap()
            .generation,
        1
    );
    assert_eq!(
        std::fs::read(root.path().join("credentials/opencode.state")).unwrap(),
        key
    );
    let backup = rusqlite::Connection::open(
        root.path()
            .join("backups/sessions-before-schema-v28.sqlite3"),
    )
    .unwrap();
    assert_eq!(
        backup
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        28
    );
    assert_eq!(
        backup
            .query_row("SELECT COUNT(*) FROM session_created_facts", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        1
    );
}
