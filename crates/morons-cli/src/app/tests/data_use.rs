use super::*;

#[test]
fn data_use_controls_are_independent_explicit_and_do_not_change_model_selection() {
    let mut app = AppState::new("test-server");
    app.information_dialog = None;
    app.install_settings(ApplicationSettings {
        subagent_model: SubagentModelSetting::InheritParent {},
        data_use: Default::default(),
    });
    let mut model = fixture_model();
    model.training_use = morons_protocol::ModelTrainingUse::NotDocumented;
    model.retention = morons_protocol::ModelRetention::NotDocumented;
    app.replace_models(model.service, vec![model.clone()])
        .unwrap();
    app.install_default_model(Some(ModelSelection {
        service: model.service,
        model_id: model.id.clone(),
    }));
    let selected = app.selected_model;
    app.open_settings_dialog();
    let text = render_rows(&mut app, 100, 30).join("\n");
    assert!(text.contains("Block training use: OFF"));
    assert!(text.contains("Require zero data retention: OFF"));
    let AppAction::SetDataUsePolicy { mut policy } =
        app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE))
    else {
        panic!("expected deliberate policy mutation")
    };
    assert!(policy.block_training_use);
    assert!(!policy.require_zero_retention);
    assert_eq!(policy.sequence, 0);
    assert!(!app.settings.as_ref().unwrap().data_use.block_training_use);
    policy.sequence = 1;
    app.install_settings(ApplicationSettings {
        subagent_model: SubagentModelSetting::InheritParent {},
        data_use: policy,
    });
    assert_eq!(app.selected_model, selected);
    assert!(app.model_policy_blocked(&model));
    app.open_settings_dialog();
    let AppAction::SetDataUsePolicy { policy } =
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE))
    else {
        panic!("expected independent retention mutation")
    };
    assert!(policy.block_training_use);
    assert!(policy.require_zero_retention);
    assert_eq!(policy.sequence, 1);
    app.open_model_dialog("");
    assert!(
        render_rows(&mut app, 100, 30)
            .join("\n")
            .contains("data-use blocked")
    );
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::None
    );
    assert!(app.model_dialog.is_some());
    assert_eq!(app.selected_model, selected);
    app.install_settings(ApplicationSettings {
        subagent_model: SubagentModelSetting::InheritParent {},
        data_use: Default::default(),
    });
    assert!(
        app.settings.as_ref().unwrap().data_use.block_training_use,
        "an older acknowledgement cannot clear newer policy presentation"
    );
}

#[test]
fn blocked_policy_retains_prompt_and_does_not_block_local_command_mode() {
    let (session, run) = fixture_session_and_run();
    let mut app = AppState::new("test-server");
    app.information_dialog = None;
    let mut model = fixture_model();
    model.training_use = morons_protocol::ModelTrainingUse::NotDocumented;
    app.replace_models(model.service, vec![model]).unwrap();
    app.open_session(session, Vec::new(), vec![run], None, None)
        .unwrap();
    app.install_settings(ApplicationSettings {
        subagent_model: SubagentModelSetting::InheritParent {},
        data_use: morons_protocol::DataUsePolicy {
            sequence: 1,
            block_training_use: true,
            require_zero_retention: false,
        },
    });
    app.handle_paste("preserve this prompt");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(app.prompt.as_str(), "preserve this prompt");
    app.prompt.clear();
    app.handle_paste("!!pwd");
    assert!(matches!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::ExecuteLocalCommand {
            context_visible: false,
            ..
        }
    ));
}
