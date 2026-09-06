use super::*;

#[test]
fn login_command_hides_credential_entry_and_emits_generation_bound_actions() {
    let (session, run) = fixture_session_and_run();
    let mut app = AppState::new("test-server");
    app.open_session(session, Vec::new(), vec![run], None, None)
        .expect("session should open");
    app.set_credential_status(OpenCodeCredentialStatus {
        configured: false,
        generation: 4,
    });
    app.handle_paste("/login");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::None
    );
    assert!(app.prompt.is_empty());
    app.handle_paste("not-a-real-key");

    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| app.render(frame))
        .expect("credential dialog should render");
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(!rendered.contains("not-a-real-key"));
    assert!(rendered.contains("Login to OpenCode"));
    assert!(rendered.contains("Input is hidden"));

    let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let debug = format!("{action:?}");
    let AppAction::SetCredential {
        expected_generation,
        api_key,
    } = action
    else {
        panic!("credential entry should produce a set action");
    };
    assert_eq!(expected_generation, 4);
    assert_eq!(
        api_key,
        OpenCodeApiKey::new("not-a-real-key").expect("test credential should be valid")
    );
    assert!(!debug.contains("not-a-real-key"));
    assert!(app.credential_dialog.is_none());
}

#[test]
fn logout_requires_confirmation_and_uses_the_observed_credential_generation() {
    let (session, run) = fixture_session_and_run();
    let mut app = AppState::new("test-server");
    app.open_session(session, Vec::new(), vec![run], None, None)
        .expect("session should open");
    app.set_credential_status(OpenCodeCredentialStatus {
        configured: true,
        generation: 9,
    });

    app.handle_paste("/logout");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::None
    );
    assert!(app.prompt.is_empty());
    assert!(matches!(
        app.credential_dialog,
        Some(CredentialDialog::ConfirmRemove)
    ));

    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| app.render(frame))
        .expect("logout confirmation should render");
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Log out of OpenCode"));
    assert!(rendered.contains("Dispatched requests"));

    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE)),
        AppAction::None
    );
    assert!(app.credential_dialog.is_none());
    app.handle_paste("/logout");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
        AppAction::RemoveCredential {
            expected_generation: 9,
        }
    );
    assert!(app.credential_dialog.is_none());

    app.set_credential_status(OpenCodeCredentialStatus {
        configured: false,
        generation: 10,
    });
    app.handle_paste("/logout");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::None
    );
    assert!(app.credential_dialog.is_none());
    assert_eq!(
        app.status.first_line(),
        "No OpenCode credential is configured"
    );
}

#[test]
fn credential_replacement_and_removal_use_observed_generation() {
    let mut app = AppState::new("test-server");
    app.set_credential_status(OpenCodeCredentialStatus {
        configured: true,
        generation: 9,
    });
    let control_k = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL);

    assert_eq!(app.handle_key(control_k), AppAction::None);
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)),
        AppAction::None
    );
    app.handle_paste("replacement-test-key");
    assert!(matches!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::SetCredential {
            expected_generation: 9,
            ..
        }
    ));

    assert_eq!(app.handle_key(control_k), AppAction::None);
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
        AppAction::RemoveCredential {
            expected_generation: 9
        }
    );
}
