use super::{AppAction, AppState};
use crate::login_link::{LinkAction, LinkEvent};
use morons_protocol::{
    OpenAiAuthorizationUrl, OpenAiCredentialState, OpenAiCredentialStatus, OpenAiLoginResult,
};
use ratatui::layout::{Position, Rect};
use ratatui_crossterm::crossterm::event::{
    KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use std::sync::Arc;

pub(crate) enum AuthEvent {
    Status(OpenAiCredentialStatus),
    Started(OpenAiAuthorizationUrl),
    Finished(OpenAiLoginResult),
    Failed(&'static str),
}
pub(crate) const UNKNOWN: &str = "Authentication outcome unknown. Reopen /login to reload status; nothing was retried. A save may have committed.";
pub(crate) fn failure_message(failure: morons_protocol::OpenAiLoginFailure) -> &'static str {
    use morons_protocol::OpenAiLoginFailure as F;
    match failure {
        F::Busy => "Another ChatGPT login is active or draining. No login was started.",
        F::CallbackUnavailable => {
            "ChatGPT callback port 1455 is unavailable. No port owner was changed."
        }
        F::Denied => "Browser authorization was denied; the existing credential was not changed.",
        F::Expired => "Browser login expired; the existing credential was not changed.",
        F::ExchangeRejected => "OpenAI rejected this exchange. Start a new login only when ready.",
        F::ExchangeUncertain => {
            "Token exchange outcome is uncertain. It was not retried; existing credentials were not replaced."
        }
        F::InvalidResponse { reason } => token_response_message(reason),
        F::CredentialChanged => {
            "Credential changed before installation. Reopen /login to reload status."
        }
        F::InstallationUncertain => UNKNOWN,
        F::Unavailable => "ChatGPT login is unavailable; nothing was retried.",
    }
}
fn token_response_message(reason: morons_protocol::OpenAiTokenResponseFailure) -> &'static str {
    use morons_protocol::OpenAiTokenResponseFailure as R;
    macro_rules! message {
        ($reason:literal) => {
            concat!(
                "ChatGPT token response rejected (",
                $reason,
                "); existing credentials were not replaced. Nothing was retried."
            )
        };
    }
    match reason {
        R::Headers => message!("headers"),
        R::BodyBounds => message!("body-bounds"),
        R::BodyFraming => message!("body-framing"),
        R::Json => message!("json"),
        R::TokenFields => message!("token-fields"),
        R::TokenType => message!("token-type"),
        R::Scope => message!("scope"),
        R::ResponseLifetime => message!("response-lifetime"),
        R::AccessTokenFormat => message!("access-token-format"),
        R::ClaimsJson => message!("claims-json"),
        R::ClaimExpiry => message!("claim-expiry"),
        R::AccountClaim => message!("account-claim"),
        R::EffectiveLifetime => message!("effective-lifetime"),
        R::Clock => message!("clock"),
        R::StoredCredential => message!("stored-credential"),
    }
}
pub(super) enum AuthDialog {
    Choose {
        logout: bool,
    },
    Loading {
        logout: bool,
    },
    Confirm {
        logout: bool,
        status: OpenAiCredentialStatus,
    },
    Waiting {
        link: Option<LoginLink>,
    },
    Removing,
    Cancelling,
    Complete(&'static str),
}
pub(crate) struct LoginLink {
    pub(crate) url: OpenAiAuthorizationUrl,
    pub(crate) scope: Arc<()>,
    message: &'static str,
}
#[derive(Clone, Copy)]
pub(super) struct LinkButtons {
    open: Rect,
    copy: Rect,
}

impl AppState {
    pub(crate) fn login_link(&self) -> Option<&LoginLink> {
        match &self.auth_dialog {
            Some(AuthDialog::Waiting { link }) => link.as_ref(),
            _ => None,
        }
    }
    pub(crate) fn handle_link_event(&mut self, event: LinkEvent) {
        if let Some(AuthDialog::Waiting { link: Some(link) }) = &mut self.auth_dialog
            && Arc::ptr_eq(&link.scope, &event.scope)
        {
            link.message = event.message;
        }
    }
    pub(super) fn handle_auth_mouse(&mut self, mouse: MouseEvent) -> AppAction {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) || self.login_link().is_none() {
            return AppAction::None;
        }
        let Some(buttons) = self.auth_link_buttons else {
            return AppAction::None;
        };
        let point = Position::new(mouse.column, mouse.row);
        let key = if buttons.open.contains(point) {
            KeyCode::Char('o')
        } else if buttons.copy.contains(point) {
            KeyCode::Char('c')
        } else {
            return AppAction::None;
        };
        self.handle_auth_key(key, KeyModifiers::NONE)
    }
    pub(super) fn handle_auth_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> AppAction {
        self.auth_link_buttons = None;
        match code {
            KeyCode::Down | KeyCode::PageDown => {
                self.auth_scroll =
                    self.auth_scroll
                        .saturating_add(if code == KeyCode::Down { 1 } else { 10 });
                return AppAction::None;
            }
            KeyCode::Up | KeyCode::PageUp => {
                self.auth_scroll =
                    self.auth_scroll
                        .saturating_sub(if code == KeyCode::Up { 1 } else { 10 });
                return AppAction::None;
            }
            KeyCode::Home => {
                self.auth_scroll = 0;
                return AppAction::None;
            }
            KeyCode::End => {
                self.auth_scroll = u16::MAX;
                return AppAction::None;
            }
            _ => {}
        }
        let cancel = code == KeyCode::Esc
            || (code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL));
        match self.auth_dialog.as_ref() {
            Some(AuthDialog::Choose { logout }) => {
                let logout = *logout;
                match code {
                    KeyCode::Char('1') => {
                        self.auth_dialog = None;
                        if logout {
                            self.open_logout_dialog();
                        } else {
                            self.open_credential_dialog();
                        }
                        AppAction::None
                    }
                    KeyCode::Char('2') => {
                        self.auth_dialog = Some(AuthDialog::Loading { logout });
                        AppAction::OpenAiStatus
                    }
                    _ if cancel => {
                        self.auth_dialog = None;
                        AppAction::None
                    }
                    _ => AppAction::None,
                }
            }
            Some(AuthDialog::Confirm { logout, status }) => {
                let (logout, status) = (*logout, *status);
                if cancel {
                    self.auth_dialog = None;
                    return AppAction::None;
                }
                if code != KeyCode::Enter {
                    return AppAction::None;
                }
                if logout {
                    if status.state == OpenAiCredentialState::Unconfigured {
                        self.auth_dialog = None;
                        AppAction::None
                    } else {
                        self.auth_dialog = Some(AuthDialog::Removing);
                        AppAction::OpenAiRemove {
                            expected_generation: status.generation,
                        }
                    }
                } else {
                    self.auth_dialog = Some(AuthDialog::Waiting { link: None });
                    AppAction::OpenAiBegin {
                        expected_generation: status.generation,
                    }
                }
            }
            Some(AuthDialog::Complete(_)) if cancel || code == KeyCode::Enter => {
                self.auth_dialog = None;
                AppAction::None
            }
            Some(
                AuthDialog::Waiting { .. } | AuthDialog::Loading { .. } | AuthDialog::Removing,
            ) if cancel => {
                self.auth_dialog = Some(AuthDialog::Cancelling);
                AppAction::OpenAiCancel
            }
            Some(AuthDialog::Waiting { link: Some(_) })
                if modifiers.is_empty() || modifiers == KeyModifiers::SHIFT =>
            {
                let action = match code {
                    KeyCode::Char('o' | 'O') | KeyCode::Enter => LinkAction::Open,
                    KeyCode::Char('c' | 'C') => LinkAction::Copy,
                    _ => return AppAction::None,
                };
                AppAction::OpenAiLink(action)
            }
            _ => AppAction::None,
        }
    }
    pub(crate) fn handle_auth_event(&mut self, event: AuthEvent) -> AppAction {
        self.auth_scroll = 0;
        self.auth_link_buttons = None;
        if self.auth_dialog.is_none() {
            return AppAction::None;
        }
        match event {
            AuthEvent::Status(status) => {
                self.auth_dialog = Some(match self.auth_dialog.as_ref() {
                    Some(AuthDialog::Loading { logout }) => AuthDialog::Confirm {
                        logout: *logout,
                        status,
                    },
                    _ => AuthDialog::Complete(
                        "Credential operation completed. Reopen /login to inspect current status. Remote authorization and dispatched work may remain.",
                    ),
                });
            }
            AuthEvent::Started(url) => {
                if matches!(self.auth_dialog, Some(AuthDialog::Waiting { link: None })) {
                    self.auth_dialog = Some(AuthDialog::Waiting {
                        link: Some(LoginLink {
                            url,
                            scope: Arc::new(()),
                            message: "Opening your default browser… Use the buttons or o/c if needed.",
                        }),
                    });
                    return AppAction::OpenAiLink(LinkAction::Open);
                }
            }
            AuthEvent::Finished(outcome) => {
                self.auth_dialog = Some(AuthDialog::Complete(match outcome {
                    OpenAiLoginResult::Installed { .. } => {
                        "ChatGPT credential installed. No model was selected. Choose GPT-5.5 (ChatGPT) deliberately when ready."
                    }
                    OpenAiLoginResult::CancelledBeforeInstallation => {
                        "Login cancelled before installation; existing credentials were not changed."
                    }
                    OpenAiLoginResult::Failed { failure } => failure_message(failure),
                }));
            }
            AuthEvent::Failed(message) => self.auth_dialog = Some(AuthDialog::Complete(message)),
        }
        AppAction::None
    }
}

pub(super) fn render(
    frame: &mut ratatui::Frame<'_>,
    dialog: &AuthDialog,
    scroll: &mut u16,
) -> Option<LinkButtons> {
    use crate::terminal::SafeText;
    use ratatui::{
        layout::Margin,
        style::{Color, Style},
        widgets::{Block, Borders, Clear, Paragraph, Wrap},
    };
    let message = match dialog {
        AuthDialog::Choose {logout} => format!("{} provider\n\n1 OpenCode (Zen/Go API key)\n2 ChatGPT subscription (browser)\n\nEsc cancel", if *logout {"Log out of"} else {"Log in to"}),
        AuthDialog::Loading {..} => "Loading ChatGPT credential status…\n\nEsc cancel".into(),
        AuthDialog::Confirm {logout,status} => format!("ChatGPT: {} · generation {}\n\n{}\n\nEnter confirm · Esc cancel", match status.state {
            OpenAiCredentialState::Unconfigured => "not configured", OpenAiCredentialState::Configured => "configured", OpenAiCredentialState::ReauthenticationRequired => "new login required",
        }, status.generation, if *logout {"Remove only the local ChatGPT credential? Remote authorization and dispatched requests may remain. This is not forensic erasure."} else {"Start browser login, replacing any current ChatGPT credential on success? Your default browser will open automatically. No other application's login is imported; no model will be selected."}),
        AuthDialog::Waiting {link: Some(link)} => format!("o/Enter open browser · c copy full link · Esc cancel\n\n{}\n\nComplete sign-in in your browser. Never paste callback URLs, codes or tokens into Morons.\n\n{}\n\nCopying exposes the URL to your clipboard and its managers. Waiting for authorization and durable installation… A save already in progress may complete after cancellation.", link.message, link.url.as_str()),
        AuthDialog::Waiting {link: None} => "Starting ChatGPT login…\n\nEsc cancel".into(),
        AuthDialog::Removing => "Removing the local ChatGPT credential…\n\nEsc waits for the outcome; removal cannot be rolled back.".into(),
        AuthDialog::Cancelling => "Cancellation requested; waiting for controlled work. An installation/removal already started may still commit. Browser authorization may remain.".into(),
        AuthDialog::Complete(message) => format!("{message}\n\nEnter/Esc close"),
    };
    let area = frame.area();
    let width = area.width.min(100);
    let height = area.height.saturating_sub(2).min(30);
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let mut content = popup.inner(Margin::new(1, 1));
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(" Auth · ↑/↓ scroll "),
        popup,
    );
    let buttons = if matches!(dialog, AuthDialog::Waiting { link: Some(_) })
        && content.width >= 8
        && content.height >= 4
    {
        let (open, copy, rows) = if content.width >= 34 {
            (
                Rect::new(content.x, content.y, 16, 1),
                Rect::new(content.x + 18, content.y, 13, 1),
                2,
            )
        } else {
            (
                Rect::new(content.x, content.y, content.width.min(16), 1),
                Rect::new(content.x, content.y + 1, content.width.min(13), 1),
                3,
            )
        };
        let style = Style::default().fg(Color::Cyan);
        frame.render_widget(
            Paragraph::new(if open.width >= 16 {
                "[o Open browser]"
            } else {
                "[o Open]"
            })
            .style(style),
            open,
        );
        frame.render_widget(
            Paragraph::new(if copy.width >= 13 {
                "[c Copy link]"
            } else {
                "[c Copy]"
            })
            .style(style),
            copy,
        );
        content.y += rows;
        content.height -= rows;
        Some(LinkButtons { open, copy })
    } else {
        None
    };
    let safe = SafeText::from_untrusted(&message);
    let paragraph = Paragraph::new(safe.as_str()).wrap(Wrap { trim: false });
    let maximum = paragraph
        .line_count(content.width.max(1))
        .saturating_sub(usize::from(content.height));
    *scroll = (*scroll).min(u16::try_from(maximum).unwrap_or(u16::MAX));
    frame.render_widget(paragraph.scroll((*scroll, 0)), content);
    buttons
}
