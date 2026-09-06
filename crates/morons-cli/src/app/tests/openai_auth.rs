use super::*;
use crate::app::auth::{AuthDialog, AuthEvent};
use morons_protocol::{
    OpenAiAuthorizationUrl, OpenAiCredentialState, OpenAiCredentialStatus, OpenAiLoginResult,
};
fn key(app: &mut AppState, code: KeyCode) -> AppAction {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
}
fn open(app: &mut AppState, logout: bool) {
    app.auth_dialog = Some(AuthDialog::Choose { logout });
    assert_eq!(key(app, KeyCode::Char('2')), AppAction::OpenAiStatus);
    app.handle_auth_event(AuthEvent::Status(OpenAiCredentialStatus {
        generation: 9,
        state: OpenAiCredentialState::ReauthenticationRequired,
    }));
}
fn url() -> OpenAiAuthorizationUrl {
    OpenAiAuthorizationUrl::new(
        "https://auth.openai.com/oauth/authorize?state=synthetic-marker".into(),
    )
    .unwrap()
}
fn render(app: &mut AppState, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| app.render(f)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}
#[test]
fn chatgpt_login_is_explicit_ephemeral_and_excluded_from_the_prompt() {
    let (session, run) = fixture_session_and_run();
    let mut app = AppState::new("test");
    app.open_session(session, Vec::new(), vec![run], None, None)
        .unwrap();
    app.handle_paste("/login");
    assert_eq!(key(&mut app, KeyCode::Enter), AppAction::None);
    assert!(matches!(app.auth_dialog, Some(AuthDialog::Choose { .. })));
    assert_eq!(key(&mut app, KeyCode::Char('2')), AppAction::OpenAiStatus);
    app.handle_auth_event(AuthEvent::Status(OpenAiCredentialStatus {
        generation: 9,
        state: OpenAiCredentialState::Configured,
    }));
    assert_eq!(
        key(&mut app, KeyCode::Enter),
        AppAction::OpenAiBegin {
            expected_generation: 9
        }
    );
    app.handle_auth_event(AuthEvent::Started(url()));
    assert!(render(&mut app, 100, 24).contains("synthetic-marker"));
    app.handle_paste("DO-NOT-IMPORT-TOKEN");
    assert_eq!(key(&mut app, KeyCode::Char('z')), AppAction::None);
    assert!(app.prompt.is_empty());
    assert!(!app.accepts_image_input());
    assert!(!render(&mut app, 100, 24).contains("DO-NOT-IMPORT-TOKEN"));
    assert_eq!(key(&mut app, KeyCode::Esc), AppAction::OpenAiCancel);
    app.handle_auth_event(AuthEvent::Started(url()));
    assert!(!render(&mut app, 100, 24).contains("synthetic-marker"));
    app.handle_auth_event(AuthEvent::Finished(
        OpenAiLoginResult::CancelledBeforeInstallation,
    ));
    assert!(!render(&mut app, 100, 24).contains("synthetic-marker"));
    assert_eq!(key(&mut app, KeyCode::Enter), AppAction::None);
    assert!(app.auth_dialog.is_none());
    assert!(app.prompt.is_empty());
    assert!(app.default_model.is_none());
}
#[test]
fn chatgpt_logout_uses_its_own_generation_and_reports_committed_cancellation_races() {
    let mut app = AppState::new("test");
    app.set_credential_status(OpenCodeCredentialStatus {
        generation: 4,
        configured: true,
    });
    open(&mut app, true);
    assert!(render(&mut app, 100, 24).contains("Remote authorization"));
    assert_eq!(
        key(&mut app, KeyCode::Enter),
        AppAction::OpenAiRemove {
            expected_generation: 9
        }
    );
    assert_eq!(key(&mut app, KeyCode::Esc), AppAction::OpenAiCancel);
    app.handle_auth_event(AuthEvent::Status(OpenAiCredentialStatus {
        generation: 10,
        state: OpenAiCredentialState::Unconfigured,
    }));
    assert!(matches!(app.auth_dialog, Some(AuthDialog::Complete(_))));
    assert_eq!(app.credential.unwrap().generation, 4);
    open(&mut app, false);
    assert_eq!(
        key(&mut app, KeyCode::Enter),
        AppAction::OpenAiBegin {
            expected_generation: 9
        }
    );
    assert_eq!(key(&mut app, KeyCode::Esc), AppAction::OpenAiCancel);
    app.handle_auth_event(AuthEvent::Finished(OpenAiLoginResult::Installed {
        generation: 10,
    }));
    assert!(render(&mut app, 100, 24).contains("credential installed"));
    assert!(app.default_model.is_none());
}
#[test]
fn authentication_dialog_is_scrollable_and_control_k_offers_both_providers() {
    let mut app = AppState::new("test");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL)),
        AppAction::None
    );
    assert!(matches!(app.auth_dialog, Some(AuthDialog::Choose { .. })));
    open(&mut app, false);
    key(&mut app, KeyCode::Enter);
    app.handle_auth_event(AuthEvent::Started(url()));
    key(&mut app, KeyCode::End);
    let _ = render(&mut app, 30, 8);
    assert!(app.auth_scroll > 0);
    key(&mut app, KeyCode::Home);
    assert_eq!(app.auth_scroll, 0);
    let _ = render(&mut app, 1, 1);
}
