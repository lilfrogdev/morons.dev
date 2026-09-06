use super::*;

#[test]
fn model_search_ranks_direct_identifier_matches_before_loose_subsequences() {
    let grok = PresentedModel::new(fixture_model());
    let mut spark = fixture_model();
    spark.id = "gpt-5.3-codex-spark".to_owned();
    spark.display_name = "GPT 5.3 Codex Spark".to_owned();
    let spark = PresentedModel::new(spark);
    assert!(
        model_search_score(&grok, "grok").expect("Grok should match")
            < model_search_score(&spark, "grok").expect("Spark is a loose fuzzy match")
    );
}

#[test]
fn slash_model_search_selects_and_preserves_one_global_default() {
    let (session, run) = fixture_session_and_run();
    let mut luna = fixture_model();
    luna.service = OpenCodeService::Go;
    luna.id = "gpt-5.6-luna".to_owned();
    luna.display_name = "GPT 5.6 Luna".to_owned();
    let mut grok = fixture_model();
    grok.service = OpenCodeService::Go;

    let mut app = AppState::new("test-server");
    app.install_default_model(Some(OpenCodeModelSelection {
        service: OpenCodeService::Go,
        model_id: "grok-4.6".to_owned(),
    }));
    app.replace_models(OpenCodeService::Go, vec![luna.clone(), grok])
        .expect("Go models should apply");
    assert_eq!(
        app.selected_model().map(|model| model.model.id.as_str()),
        Some("grok-4.6")
    );
    app.open_session(session, Vec::new(), vec![run], None, None)
        .expect("session should open without restoring its historical model");
    assert_eq!(
        app.selected_model().map(|model| model.model.id.as_str()),
        Some("grok-4.6")
    );

    app.handle_paste("/model");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(
        app.model_dialog.as_ref().map(|dialog| dialog.selected),
        Some(1)
    );
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(
        app.model_dialog.as_ref().map(|dialog| dialog.selected),
        Some(0)
    );
    app.handle_paste(&"x".repeat(MAX_MODEL_SEARCH_BYTES + 20));
    assert_eq!(
        app.model_dialog
            .as_ref()
            .map(|dialog| dialog.query.len_bytes()),
        Some(MAX_MODEL_SEARCH_BYTES)
    );
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        AppAction::None
    );
    assert!(app.model_dialog.is_none());
    assert_eq!(
        app.selected_model().map(|model| model.model.id.as_str()),
        Some("grok-4.6")
    );

    app.handle_paste("/model luna");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::None
    );
    assert!(app.prompt.is_empty());
    assert_eq!(
        app.model_dialog
            .as_ref()
            .map(|dialog| dialog.query.as_str()),
        Some("luna")
    );
    assert_eq!(app.model_dialog_matches(), vec![0]);

    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| app.render(frame))
        .expect("model selector should render");
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Model search"));
    assert!(rendered.contains("gpt-5.6-luna"));
    assert!(!rendered.contains("grok-4.6"));
    for (width, height) in [(1, 1), (8, 3), (40, 10)] {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("small terminal should initialize");
        terminal
            .draw(|frame| app.render(frame))
            .expect("model selector should render in a small terminal");
    }

    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        AppAction::None
    );
    let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        action,
        AppAction::SetDefaultModel {
            service: OpenCodeService::Go,
            ref model_id,
        } if model_id == "gpt-5.6-luna"
    ));
    app.install_default_model(Some(OpenCodeModelSelection {
        service: OpenCodeService::Go,
        model_id: luna.id,
    }));
    app.close_session();
    let selected_before_tab = app.selected_model;
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(app.selected_model, selected_before_tab);
}

#[test]
fn slash_settings_selects_an_independent_reviewed_subagent_model_or_parent_inheritance() {
    let (session, run) = fixture_session_and_run();
    let mut glm = fixture_model();
    glm.service = OpenCodeService::Go;
    glm.id = "glm-5.3-flash".to_owned();
    glm.display_name = "GLM-5.3-Flash".to_owned();
    glm.protocol = ProviderProtocol::ChatCompletions;
    glm.protocol_revision = 2;

    let mut app = AppState::new("test-server");
    app.information_dialog = None;
    app.install_settings(ApplicationSettings {
        subagent_model: SubagentModelSetting::InheritParent {},
    });
    app.replace_models(OpenCodeService::Go, vec![glm])
        .expect("Go models should apply");
    app.open_session(session, Vec::new(), vec![run], None, None)
        .expect("session should open");

    app.handle_paste("/settings");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::LoadSettings
    );
    assert!(app.prompt.is_empty());
    assert!(matches!(
        app.settings_dialog,
        Some(SettingsDialog::Overview)
    ));
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::None
    );
    assert!(matches!(
        app.settings_dialog,
        Some(SettingsDialog::SubagentModel { selected: 0, .. })
    ));
    app.handle_paste("glm");
    assert_eq!(
        app.subagent_model_dialog_matches(),
        vec![SubagentModelCandidate::Model(0)]
    );

    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| app.render(frame))
        .expect("settings should render");
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Subagent model search"));
    assert!(rendered.contains("glm-5.3-flash"));
    assert!(rendered.contains("protocol 2"));
    assert!(rendered.contains("training: not used"));

    let selected = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        selected,
        AppAction::SetSubagentModel {
            setting: SubagentModelSetting::OpenCode {
                service: OpenCodeService::Go,
                ref model_id,
            },
        } if model_id == "glm-5.3-flash"
    ));
    app.install_settings(ApplicationSettings {
        subagent_model: SubagentModelSetting::OpenCode {
            service: OpenCodeService::Go,
            model_id: "glm-5.3-flash".to_owned(),
        },
    });
    app.open_settings_dialog();
    app.open_subagent_model_dialog();
    assert!(matches!(
        app.settings_dialog,
        Some(SettingsDialog::SubagentModel { selected: 1, .. })
    ));
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::SetSubagentModel {
            setting: SubagentModelSetting::InheritParent {},
        }
    );
}

#[test]
fn unavailable_saved_default_falls_back_only_to_an_available_reviewed_model() {
    let mut available = fixture_model();
    available.service = OpenCodeService::Go;
    available.id = "gpt-5.6-luna".to_owned();
    available.display_name = "GPT 5.6 Luna".to_owned();
    let mut app = AppState::new("test-server");
    app.install_default_model(Some(OpenCodeModelSelection {
        service: OpenCodeService::Go,
        model_id: "grok-4.6".to_owned(),
    }));
    app.replace_models(OpenCodeService::Go, vec![available.clone()])
        .expect("Go models should apply");
    assert!(app.default_model_is_unavailable());
    assert_eq!(
        app.selected_model().map(|model| model.model.id.as_str()),
        Some(available.id.as_str())
    );
}

#[test]
fn selected_model_is_always_an_available_reviewed_pair() {
    let mut unavailable = fixture_model();
    unavailable.available = false;
    let available = OpenCodeModelSummary {
        service: OpenCodeService::Go,
        id: "gpt-5.6-luna".to_owned(),
        display_name: "GPT 5.6 Luna".to_owned(),
        available: true,
        ..fixture_model()
    };
    let mut app = AppState::new("test-server");
    app.replace_models(OpenCodeService::Zen, vec![unavailable])
        .expect("Zen models should apply");
    assert!(app.selected_model().is_none());
    app.replace_models(OpenCodeService::Go, vec![available.clone()])
        .expect("Go models should apply");
    let selected = app
        .selected_model()
        .expect("available model should be selected");
    assert_eq!(selected.model.service, OpenCodeService::Go);
    assert_eq!(selected.model.id, available.id);
    app.replace_models(OpenCodeService::Go, Vec::new())
        .expect("a failed catalog can remove stale availability");
    assert!(app.selected_model().is_none());
}
