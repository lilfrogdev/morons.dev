use super::*;

#[tokio::test(flavor = "current_thread")]
async fn model_catalog_query_returns_only_reviewed_server_metadata() {
    let root = TestRoot::new("model-catalog-query");
    let store = SessionStore::open_for_test(root.path()).expect("session store should open");
    let (base, captured_request, server) = spawn_catalog_provider().await;
    let application = ServerApplication::from_session_store_for_test(store, &base);

    let outcome = application
        .execute_for_local_owner(ApplicationRequest::ListModels {
            service: ModelService::Go,
        })
        .await
        .expect("model catalog query should succeed");
    let ApplicationOutcome::Response(ApplicationResponse::ModelsListed { service, models }) =
        outcome
    else {
        panic!("model catalog query should return model summaries");
    };
    assert_eq!(service, ModelService::Go);
    assert!(models.iter().all(|model| model.service == service));
    assert!(
        models
            .iter()
            .find(|model| model.id == "gpt-5.6-luna")
            .expect("reviewed model should be returned")
            .available
    );
    assert!(models.iter().any(|model| {
        model.id == "glm-5.3-flash"
            && model.available
            && model.protocol == morons_protocol::ProviderProtocol::ChatCompletions
            && model.protocol_revision == crate::provider::CHAT_COMPLETIONS_PROTOCOL_REVISION
    }));
    assert_eq!(models.len(), 35);
    assert!(models.iter().any(|model| {
        model.id == "muse-spark-1.2-contributor"
            && model.available
            && model.training_use == morons_protocol::ModelTrainingUse::MayUsePromptsAndCompletions
            && model.retention == morons_protocol::ModelRetention::NotZeroDataRetention
    }));
    assert!(models.iter().any(|model| {
        model.id == "qwen3.8-max"
            && model.available
            && model.protocol == morons_protocol::ProviderProtocol::AnthropicMessages
            && model.protocol_revision == crate::provider::ANTHROPIC_MESSAGES_PROTOCOL_REVISION
    }));
    assert!(models.iter().any(|model| {
        model.id == "grok-4.5"
            && !model.available
            && model.training_use == morons_protocol::ModelTrainingUse::NotDocumented
            && model.retention == morons_protocol::ModelRetention::NotDocumented
    }));

    let captured = captured_request
        .await
        .expect("catalog request should be captured");
    assert!(captured.starts_with("GET /zen/go/v1/models HTTP/1.1"));
    assert!(!captured.to_ascii_lowercase().contains("authorization:"));
    server.await.expect("catalog fixture should finish");
    application.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn default_model_selection_is_reviewed_idempotent_and_queryable() {
    let root = TestRoot::new("default-model-application");
    let store = SessionStore::open_for_test(root.path()).expect("session store should open");
    let application = ServerApplication::from_session_store_for_test(store, "http://127.0.0.1:9");

    let empty = application
        .execute_for_local_owner(ApplicationRequest::GetDefaultModel)
        .await
        .expect("empty default query should succeed");
    assert!(matches!(
        empty,
        ApplicationOutcome::Response(ApplicationResponse::DefaultModel { selection: None })
    ));

    let request = ApplicationRequest::SetDefaultModel {
        mutation_request_id: MutationRequestId::from_bytes([0x61; 16]),
        service: ModelService::Go,
        model_id: "grok-4.6".to_owned(),
    };
    for _ in 0..2 {
        let selected = application
            .execute_for_local_owner(request.clone())
            .await
            .expect("default selection should succeed");
        assert!(matches!(
            selected,
            ApplicationOutcome::Response(ApplicationResponse::DefaultModelUpdated {
                selection
            }) if selection.service == ModelService::Go && selection.model_id == "grok-4.6"
        ));
    }
    let loaded = application
        .execute_for_local_owner(ApplicationRequest::GetDefaultModel)
        .await
        .expect("selected default should be queried");
    assert!(matches!(
        loaded,
        ApplicationOutcome::Response(ApplicationResponse::DefaultModel {
            selection: Some(selection)
        }) if selection.service == ModelService::Go && selection.model_id == "grok-4.6"
    ));

    let unsupported = application
        .execute_for_local_owner(ApplicationRequest::SetDefaultModel {
            mutation_request_id: MutationRequestId::from_bytes([0x62; 16]),
            service: ModelService::Go,
            model_id: "not-reviewed".to_owned(),
        })
        .await;
    assert!(matches!(
        unsupported,
        Err(ApplicationError::UnsupportedModel)
    ));
    application.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn subagent_model_setting_is_reviewed_idempotent_and_queryable() {
    let root = TestRoot::new("subagent-model-application");
    let store = SessionStore::open_for_test(root.path()).expect("session store should open");
    let application = ServerApplication::from_session_store_for_test(store, "http://127.0.0.1:9");

    let initial = application
        .execute_for_local_owner(ApplicationRequest::GetApplicationSettings)
        .await
        .expect("initial settings query should succeed");
    assert!(matches!(
        initial,
        ApplicationOutcome::Response(ApplicationResponse::ApplicationSettings { settings })
            if settings.subagent_model == SubagentModelSetting::InheritParent {}
    ));

    let request = ApplicationRequest::SetSubagentModelSetting {
        mutation_request_id: MutationRequestId::from_bytes([0x63; 16]),
        setting: SubagentModelSetting::Explicit {
            service: ModelService::Go,
            model_id: "glm-5.3-flash".to_owned(),
        },
    };
    for _ in 0..2 {
        let updated = application
            .execute_for_local_owner(request.clone())
            .await
            .expect("subagent model setting should succeed");
        assert!(matches!(
            updated,
            ApplicationOutcome::Response(ApplicationResponse::ApplicationSettingsUpdated {
                settings
            }) if matches!(
                settings.subagent_model,
                SubagentModelSetting::Explicit {
                    service: ModelService::Go,
                    ref model_id,
                } if model_id == "glm-5.3-flash"
            )
        ));
    }
    let loaded = application
        .execute_for_local_owner(ApplicationRequest::GetApplicationSettings)
        .await
        .expect("updated settings should load");
    assert!(matches!(
        loaded,
        ApplicationOutcome::Response(ApplicationResponse::ApplicationSettings { settings })
            if settings.subagent_model == request_setting(&request)
    ));

    let unsupported = application
        .execute_for_local_owner(ApplicationRequest::SetSubagentModelSetting {
            mutation_request_id: MutationRequestId::from_bytes([0x64; 16]),
            setting: SubagentModelSetting::Explicit {
                service: ModelService::Go,
                model_id: "not-reviewed".to_owned(),
            },
        })
        .await;
    assert!(matches!(
        unsupported,
        Err(ApplicationError::UnsupportedModel)
    ));
    application.shutdown().await;
}
