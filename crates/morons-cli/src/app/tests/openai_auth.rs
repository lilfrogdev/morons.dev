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
fn token_validation_failure_reasons_clear_links_without_retry_or_selection() {
    use morons_protocol::{OpenAiLoginFailure as F, OpenAiTokenResponseFailure as R};
    for (reason, label) in [
        (R::Headers, "headers"),
        (R::BodyBounds, "body-bounds"),
        (R::BodyFraming, "body-framing"),
        (R::Json, "json"),
        (R::TokenFields, "token-fields"),
        (R::TokenType, "token-type"),
        (R::Scope, "scope"),
        (R::ResponseLifetime, "response-lifetime"),
        (R::AccessTokenFormat, "access-token-format"),
        (R::ClaimsJson, "claims-json"),
        (R::ClaimExpiry, "claim-expiry"),
        (R::AccountClaim, "account-claim"),
        (R::EffectiveLifetime, "effective-lifetime"),
        (R::Clock, "clock"),
        (R::StoredCredential, "stored-credential"),
    ] {
        let mut app = AppState::new("test");
        open(&mut app, false);
        key(&mut app, KeyCode::Enter);
        app.handle_auth_event(AuthEvent::Started(url()));
        assert!(app.login_link().is_some());
        assert_eq!(
            app.handle_auth_event(AuthEvent::Finished(OpenAiLoginResult::Failed {
                failure: F::InvalidResponse { reason },
            })),
            AppAction::None
        );
        assert!(app.login_link().is_none());
        let Some(AuthDialog::Complete(message)) = app.auth_dialog else {
            panic!("expected failure dialog")
        };
        assert!(message.contains(&format!("({label})")));
        assert!(message.contains("existing credentials were not replaced"));
        assert!(message.contains("Nothing was retried"));
        assert!(!render(&mut app, 120, 30).contains("synthetic-marker"));
        assert_eq!(key(&mut app, KeyCode::Enter), AppAction::None);
        assert!(app.prompt.is_empty());
        assert!(app.default_model.is_none());
    }
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

#[test]
fn login_link_auto_open_is_once_only_and_late_outcomes_cannot_rebind_the_dialog() {
    use crate::login_link::{LinkAction, LinkEvent};
    let mut app = AppState::new("test");
    open(&mut app, false);
    let notice = render(&mut app, 100, 24);
    assert!(notice.contains("default browser"));
    assert!(!notice.contains("not enabled yet"));
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        app.handle_auth_event(AuthEvent::Started(url())),
        AppAction::OpenAiLink(LinkAction::Open)
    );
    let old_scope = std::sync::Arc::clone(&app.login_link().unwrap().scope);
    assert_eq!(
        app.handle_auth_event(AuthEvent::Started(url())),
        AppAction::None
    );
    assert_eq!(
        key(&mut app, KeyCode::Char('c')),
        AppAction::OpenAiLink(LinkAction::Copy)
    );
    assert_eq!(
        key(&mut app, KeyCode::Enter),
        AppAction::OpenAiLink(LinkAction::Open)
    );
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        AppAction::OpenAiCancel
    );
    assert!(app.login_link().is_none());
    assert_eq!(
        app.handle_auth_event(AuthEvent::Started(url())),
        AppAction::None
    );
    app.handle_auth_event(AuthEvent::Finished(
        OpenAiLoginResult::CancelledBeforeInstallation,
    ));
    open(&mut app, false);
    key(&mut app, KeyCode::Enter);
    app.handle_auth_event(AuthEvent::Started(url()));
    app.handle_link_event(LinkEvent {
        scope: old_scope,
        message: "STALE-LINK-RESULT",
    });
    assert!(!render(&mut app, 100, 24).contains("STALE-LINK-RESULT"));
    let scope = std::sync::Arc::clone(&app.login_link().unwrap().scope);
    app.handle_link_event(LinkEvent {
        scope,
        message: "CURRENT-LINK-RESULT",
    });
    assert!(render(&mut app, 100, 24).contains("CURRENT-LINK-RESULT"));
    app.handle_auth_event(AuthEvent::Finished(OpenAiLoginResult::Installed {
        generation: 10,
    }));
    let complete = render(&mut app, 100, 24);
    assert!(!complete.contains("not enabled yet"));
    assert!(!complete.contains("synthetic-marker"));
    assert!(app.default_model.is_none());
    assert_eq!(key(&mut app, KeyCode::Char('c')), AppAction::None);
}

#[test]
fn login_link_buttons_support_plain_and_control_click_and_narrow_layouts() {
    use crate::login_link::LinkAction;
    use ratatui_crossterm::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut app = AppState::new("test");
    open(&mut app, false);
    key(&mut app, KeyCode::Enter);
    app.handle_auth_event(AuthEvent::Started(url()));
    for (width, height, copy_x, copy_y) in [(100, 24, 19, 2), (30, 8, 1, 3)] {
        for modifiers in [KeyModifiers::NONE, KeyModifiers::CONTROL] {
            render(&mut app, width, height);
            assert_eq!(
                app.handle_mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 1,
                    row: 2,
                    modifiers
                }),
                AppAction::OpenAiLink(LinkAction::Open)
            );
            render(&mut app, width, height);
            assert_eq!(
                app.handle_mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: copy_x,
                    row: copy_y,
                    modifiers
                }),
                AppAction::OpenAiLink(LinkAction::Copy)
            );
        }
    }
    render(&mut app, 1, 1);
    assert!(app.auth_link_buttons.is_none());
    assert_eq!(
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 2,
            modifiers: KeyModifiers::NONE
        }),
        AppAction::None
    );
    render(&mut app, 100, 24);
    key(&mut app, KeyCode::Esc);
    assert_eq!(
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 2,
            modifiers: KeyModifiers::NONE
        }),
        AppAction::None
    );
    assert!(app.prompt.is_empty());
}
