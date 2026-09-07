use super::*;

#[tokio::test(flavor = "current_thread")]
async fn native_model_admission_is_reviewed_policy_checked_and_uses_separate_login_errors() {
    let root = TestRoot::new("native-admission");
    let selected = TestRoot::new("native-admission-selected");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0x91; 16]),
            None,
            selected.path().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    let app = ServerApplication::from_native_store_for_test(store, "http://127.0.0.1:9");
    let response = app
        .execute_for_local_owner(ApplicationRequest::ListModels {
            service: ModelService::OpenAiChatGpt,
        })
        .await
        .unwrap();
    let ApplicationOutcome::Response(ApplicationResponse::ModelsListed { models, .. }) = response
    else {
        panic!("expected model metadata")
    };
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "gpt-5.5");
    assert!(models[0].output_limit_is_local);
    assert_eq!(models[0].protocol_revision, 5);
    assert!(matches!(
        app.execute_for_local_owner(ApplicationRequest::SetDefaultModel {
            mutation_request_id: MutationRequestId::from_bytes([0x92; 16]),
            service: ModelService::OpenAiChatGpt,
            model_id: "gpt-5.4".into()
        })
        .await,
        Err(ApplicationError::UnsupportedModel)
    ));
    let mut sequence = 0;
    for (index, (training, retention)) in
        [(false, false), (true, false), (false, true), (true, true)]
            .into_iter()
            .enumerate()
    {
        let response = app
            .execute_for_local_owner(ApplicationRequest::SetDataUsePolicy {
                mutation_request_id: MutationRequestId::from_bytes(
                    [0xa0 + u8::try_from(index).unwrap(); 16],
                ),
                policy: morons_protocol::DataUsePolicy {
                    sequence,
                    block_training_use: training,
                    require_zero_retention: retention,
                },
            })
            .await
            .unwrap();
        let ApplicationOutcome::Response(ApplicationResponse::ApplicationSettingsUpdated {
            settings,
        }) = response
        else {
            panic!("expected settings")
        };
        sequence = settings.data_use.sequence;
        let selection = app
            .execute_for_local_owner(ApplicationRequest::SetDefaultModel {
                mutation_request_id: MutationRequestId::from_bytes(
                    [0xb0 + u8::try_from(index).unwrap(); 16],
                ),
                service: ModelService::OpenAiChatGpt,
                model_id: "gpt-5.5".into(),
            })
            .await;
        assert_eq!(selection.is_ok(), !training && !retention);
        let admission = app
            .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
                mutation_request_id: MutationRequestId::from_bytes(
                    [0xc0 + u8::try_from(index).unwrap(); 16],
                ),
                session_id: SessionId::from_bytes(*session.id.as_bytes()),
                text: "Do not dispatch".into(),
                attachments: Vec::new(),
                service: ModelService::OpenAiChatGpt,
                model_id: "gpt-5.5".into(),
            })
            .await;
        if training || retention {
            assert!(matches!(
                admission,
                Err(ApplicationError::DataUseRestricted)
            ));
        } else {
            assert!(matches!(
                admission,
                Err(ApplicationError::OpenAiCredentialNotConfigured)
            ));
        }
    }
    app.shutdown().await;
    let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM run_accepted_facts", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
