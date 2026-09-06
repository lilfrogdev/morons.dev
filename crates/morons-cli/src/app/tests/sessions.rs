use super::*;

#[test]
fn session_browser_presents_the_bound_working_directory() {
    let (mut session, _) = fixture_session_and_run();
    session.working_directory = Some("/projects/example".to_owned());
    let mut other = session.clone();
    other.id = SessionId::from_bytes([0x77; 16]);
    other.display_name = Some("Same directory".to_owned());
    let mut app = AppState::new("test-server");
    app.replace_sessions(vec![session, other])
        .expect("sessions should be presented");

    let backend = TestBackend::new(100, 12);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| app.render(frame))
        .expect("session browser should render");
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("/projects/example"));
    assert!(rendered.contains("shared directory"));
    assert!(app.sessions.iter().all(|session| session.shared_directory));
}

#[test]
fn session_browser_rename_is_bounded_modal_and_redacted() {
    let (session, _) = fixture_session_and_run();
    let session_id = session.id;
    let mut app = AppState::new("test-server");
    app.replace_sessions(vec![session])
        .expect("session should be presented");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)),
        AppAction::None
    );
    assert!(app.rename_dialog.is_some());
    app.handle_paste("Renamed session");
    let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        action,
        AppAction::RenameSession {
            session_id: selected,
            ref display_name,
        } if selected == session_id && display_name == "Renamed session"
    ));
    assert!(!format!("{action:?}").contains("Renamed session"));
    assert!(app.rename_dialog.is_none());
}

#[test]
fn session_browser_toggles_durable_archive_state() {
    let (session, _) = fixture_session_and_run();
    let session_id = session.id;
    let mut app = AppState::new("test-server");
    app.replace_sessions(vec![session])
        .expect("session should be presented");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
        AppAction::SetSessionArchived {
            session_id,
            archived: true,
        }
    );
    app.sessions[0].summary.archived = true;
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
        AppAction::SetSessionArchived {
            session_id,
            archived: false,
        }
    );
}

#[test]
fn session_browser_requires_archived_confirmation_before_deletion() {
    let (mut session, _) = fixture_session_and_run();
    let session_id = session.id;
    session.archived = true;
    let mut app = AppState::new("test-server");
    app.replace_sessions(vec![session])
        .expect("session should be presented");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(app.confirm_delete, Some(session_id));
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
        AppAction::DeleteSession { session_id }
    );
    assert_eq!(app.confirm_delete, None);
}
