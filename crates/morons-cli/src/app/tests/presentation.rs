use super::*;

#[test]
fn trusted_local_onboarding_and_help_are_explicit_and_modal() {
    let mut app = AppState::new("test-server");
    app.information_dialog = Some(InformationDialog::TrustNotice);
    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| app.render(frame))
        .expect("trust notice should render");
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("not a sandbox"));
    assert!(rendered.contains("container, VM"));
    assert!(rendered.contains("restricted OS account"));
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::None
    );
    assert!(app.information_dialog.is_none());
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(app.information_dialog, Some(InformationDialog::Help));
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        AppAction::None
    );
    assert!(app.information_dialog.is_none());
}

#[test]
fn rendering_strips_terminal_and_bidirectional_controls() {
    let (session, run) = fixture_session_and_run();
    let mut app = AppState::new("test-server");
    app.replace_models(OpenCodeService::Zen, vec![fixture_model()])
        .expect("models should be valid");
    app.open_session(
        session,
        vec![TranscriptEntry::UserMessage {
            id: run.user_message_id,
            run_id: run.id,
            text: "safe\u{1b}]8;;https://example.invalid\u{7}link\u{1b}]8;;\u{7}\u{202e}txt"
                .to_owned(),
            attachments: Vec::new(),
            created_at_milliseconds: 1,
        }],
        vec![run],
        None,
        None,
    )
    .expect("session should open");

    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| app.render(frame))
        .expect("application should render");
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();

    assert!(!rendered.contains('\u{1b}'));
    assert!(!rendered.contains('\u{202e}'));
    assert!(rendered.contains("safelinktxt"));
}
