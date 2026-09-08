use super::*;

#[test]
fn native_selection_is_deliberate_discloses_local_limits_and_remains_policy_blockable() {
    check_native_selection("gpt-5.5", "GPT-5.5 (ChatGPT)");
}

#[test]
fn requested_native_models_are_exact_deliberate_selections_with_daybreak_disclosure() {
    for (id, name) in [
        ("gpt-6-astra", "GPT-6 Astra (ChatGPT)"),
        ("gpt-5.6-sol", "GPT-5.6 Sol (ChatGPT)"),
        ("gpt-5.6-luna", "GPT-5.6 Luna (ChatGPT)"),
        ("gpt-5.6-terra", "GPT-5.6 Terra (ChatGPT)"),
        (
            "gpt-daybreak-blue-latest",
            "Daybreak Blue (ChatGPT; approval required)",
        ),
    ] {
        check_native_selection(id, name);
    }
}

fn check_native_selection(id: &str, name: &str) {
    let mut app = AppState::new("test-server");
    app.information_dialog = None;
    app.install_settings(ApplicationSettings {
        subagent_model: SubagentModelSetting::InheritParent {},
        data_use: Default::default(),
    });
    let mut model = fixture_model();
    model.service = ModelService::OpenAiChatGpt;
    model.id = id.into();
    model.display_name = name.into();
    model.protocol_revision = 5;
    model.output_limit_is_local = true;
    model.training_use = morons_protocol::ModelTrainingUse::NotDocumented;
    model.retention = morons_protocol::ModelRetention::NotDocumented;
    app.replace_models(ModelService::OpenAiChatGpt, vec![model])
        .unwrap();
    assert!(
        app.selected_model().is_none(),
        "native availability must not cause implicit provider selection"
    );
    app.open_model_dialog("");
    let rendered = render_rows(&mut app, 100, 30).join("\n");
    assert!(rendered.contains("Local output limit only"));
    assert!(rendered.contains("not proof of subscription entitlement"));
    if id == "gpt-daybreak-blue-latest" {
        assert!(rendered.contains("approval required"));
    }
    assert!(matches!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::SetDefaultModel {
            service: ModelService::OpenAiChatGpt,
            model_id,
        } if model_id == id
    ));
    assert!(
        app.selected_model().is_none(),
        "selection waits for server acknowledgement"
    );
    app.install_default_model(Some(ModelSelection {
        service: ModelService::OpenAiChatGpt,
        model_id: id.into(),
    }));
    assert_eq!(
        app.selected_model().unwrap().model.service,
        ModelService::OpenAiChatGpt
    );
    app.install_settings(ApplicationSettings {
        subagent_model: SubagentModelSetting::InheritParent {},
        data_use: morons_protocol::DataUsePolicy {
            sequence: 1,
            block_training_use: true,
            require_zero_retention: false,
        },
    });
    app.open_model_dialog("");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(
        app.selected_model().unwrap().model.service,
        ModelService::OpenAiChatGpt
    );
}
