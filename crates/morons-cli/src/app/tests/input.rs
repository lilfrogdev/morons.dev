use super::*;

#[test]
fn prompt_paste_and_rendering_remain_bounded() {
    let (session, run) = fixture_session_and_run();
    let mut app = AppState::new("test-server");
    app.open_session(session, Vec::new(), vec![run], None, None)
        .expect("session should open");
    app.handle_paste(&format!(
        "{}\u{1b}\u{202e}",
        "x".repeat(MAX_PROMPT_BYTES + 100)
    ));
    assert_eq!(app.prompt.len_bytes(), MAX_PROMPT_BYTES);
    assert!(!app.prompt.as_str().contains('\u{1b}'));
    assert!(!app.prompt.as_str().contains('\u{202e}'));

    for (width, height) in [(1, 1), (8, 3), (40, 10)] {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
        terminal
            .draw(|frame| app.render(frame))
            .expect("small terminal should render without panic");
    }
}

#[test]
fn bang_prompts_execute_local_commands_without_requiring_a_model() {
    let (session, run) = fixture_session_and_run();
    let session_id = session.id;
    let mut app = AppState::new("test-server");
    app.open_session(session, Vec::new(), vec![run], None, None)
        .expect("session should open");
    app.handle_paste("!!  printf private");
    let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        action,
        AppAction::ExecuteLocalCommand {
            session_id: selected,
            ref command,
            context_visible: false,
        } if selected == session_id && command == "printf private"
    ));
    assert!(!format!("{action:?}").contains("printf private"));
}

#[test]
fn slash_context_controls_query_status_and_submit_manual_compaction() {
    let (session, run) = fixture_session_and_run();
    let session_id = session.id;
    let mut app = AppState::new("test-server");
    app.replace_models(OpenCodeService::Zen, vec![fixture_model()])
        .expect("models should be valid");
    app.open_session(session, Vec::new(), vec![run], None, None)
        .expect("session should open");

    app.handle_paste("/context");
    assert!(matches!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::ShowContext {
            session_id: selected,
            service: OpenCodeService::Zen,
            ref model_id,
        } if selected == session_id && model_id == "grok-4.6"
    ));
    app.context_status_loaded(SessionContextStatus {
        session_id,
        service: OpenCodeService::Zen,
        model_id: "grok-4.6".to_owned(),
        context_policy_version: 4,
        estimated_input_tokens: 12_000,
        maximum_input_tokens: 96_000,
        maximum_output_tokens: 32_000,
        compaction_threshold_tokens: 67_200,
        checkpoint_source_entry_high_water: Some(7),
        checkpoint_estimated_summary_tokens: Some(500),
    })
    .expect("context status should install");
    assert!(app.prompt.is_empty());
    assert_eq!(
        app.session
            .as_ref()
            .and_then(|session| session.context_status.as_ref())
            .map(|context| context.estimated_input_tokens),
        Some(12_000)
    );

    app.handle_paste("/compact preserve migration constraints");
    assert!(matches!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::SubmitInput {
            session_id: selected,
            ref text,
            ref attachments,
            ..
        } if selected == session_id
            && text == "/compact preserve migration constraints"
            && attachments.is_empty()
    ));
}

#[test]
fn at_prefix_opens_bounded_skill_completion_and_tab_inserts_exact_name() {
    let (session, run) = fixture_session_and_run();
    let session_id = session.id;
    let mut app = AppState::new("test-server");
    app.replace_models(OpenCodeService::Zen, vec![fixture_model()])
        .expect("models should be valid");
    app.open_session(session, Vec::new(), vec![run], None, None)
        .expect("session should open");
    app.install_session_skills(
        session_id,
        vec![
            SkillSummary {
                name: "alpha".to_owned(),
                description: "Alpha workflow".to_owned(),
                source: SkillSource::Project,
            },
            SkillSummary {
                name: "skill-creator".to_owned(),
                description: "Create Agent Skills".to_owned(),
                source: SkillSource::Bundled,
            },
        ],
    )
    .expect("skills should install");
    app.handle_paste("@");
    assert_eq!(
        app.skill_completion().map(|(skills, _)| skills.len()),
        Some(2)
    );

    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| app.render(frame))
        .expect("skill completion should render");
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("@alpha"));
    assert!(rendered.contains("@skill-creator"));

    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(app.prompt.as_str(), "@skill-creator ");
    app.handle_paste("make a release skill");
    let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        action,
        AppAction::SubmitInput {
            session_id: selected,
            ref text,
            ..
        } if selected == session_id && text == "@skill-creator make a release skill"
    ));
}

#[test]
fn image_drafts_use_atomic_unique_markers_and_survive_unsupported_submission() {
    let (session, run) = fixture_session_and_run();
    let session_id = session.id;
    let mut app = AppState::new("test-server");
    app.replace_models(OpenCodeService::Zen, vec![fixture_model()])
        .expect("models should be valid");
    app.open_session(session, Vec::new(), vec![run], None, None)
        .expect("session should open");
    let image =
        morons_image::normalize_rgba(2, 2, vec![0x55; 16]).expect("fixture image should normalize");
    assert_eq!(
        sanitize_image_name("bad\u{202e}[name].png"),
        "bad__name_.png"
    );
    app.add_draft_image(image.clone(), Some("puppies.png"));
    app.add_draft_image(image, Some("puppies.png"));
    assert_eq!(app.prompt.as_str(), "[puppies.png][puppies (2).png]");
    app.backspace_prompt();
    assert_eq!(app.prompt.as_str(), "[puppies.png]");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(app.prompt.as_str(), "[puppies.png]");

    app.models[0].model.capabilities.image_input = true;
    let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let debug = format!("{action:?}");
    assert!(matches!(
        action,
        AppAction::SubmitInput {
            session_id: selected,
            ref attachments,
            ..
        } if selected == session_id
            && attachments.len() == 1
            && attachments[0].display_name == "puppies.png"
            && attachments[0].marker_start == 0
    ));
    assert!(!debug.contains("iVBOR"));
    assert!(debug.contains("attachments"));
}

#[test]
fn unknown_mutation_requires_exact_retry_or_abandonment() {
    let mut app = AppState::new("test-server");
    app.mark_pending(PendingOperation::CreateSession);
    app.mark_pending_unknown();

    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)),
        AppAction::RetryPending
    );
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
        AppAction::AbandonPending
    );
}

#[test]
fn input_action_debug_omits_prompt_text() {
    let action = AppAction::SubmitInput {
        session_id: SessionId::from_bytes([0x55; 16]),
        text: "sensitive prompt text".to_owned(),
        attachments: Vec::new(),
        service: OpenCodeService::Zen,
        model_id: "grok-4.6".to_owned(),
    };
    let debug = format!("{action:?}");
    assert!(!debug.contains("sensitive prompt text"));
    assert!(debug.contains("text_bytes"));
}
