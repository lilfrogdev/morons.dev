use super::{AppAction, AppState};
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
        F::InvalidResponse => {
            "OpenAI returned an invalid token response; existing credentials were not replaced."
        }
        F::CredentialChanged => {
            "Credential changed before installation. Reopen /login to reload status."
        }
        F::InstallationUncertain => UNKNOWN,
        F::Unavailable => "ChatGPT login is unavailable; nothing was retried.",
    }
}
use morons_protocol::{
    OpenAiAuthorizationUrl, OpenAiCredentialState, OpenAiCredentialStatus, OpenAiLoginResult,
};
use ratatui_crossterm::crossterm::event::{KeyCode, KeyModifiers};

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
        url: Option<OpenAiAuthorizationUrl>,
    },
    Removing,
    Cancelling,
    Complete(&'static str),
}
impl AppState {
    pub(super) fn handle_auth_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> AppAction {
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
                    self.auth_dialog = Some(AuthDialog::Waiting { url: None });
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
            _ => AppAction::None,
        }
    }
    pub(crate) fn handle_auth_event(&mut self, event: AuthEvent) {
        self.auth_scroll = 0;
        if self.auth_dialog.is_none() {
            return;
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
                if matches!(self.auth_dialog, Some(AuthDialog::Waiting { .. })) {
                    self.auth_dialog = Some(AuthDialog::Waiting { url: Some(url) });
                }
            }
            AuthEvent::Finished(outcome) => {
                self.auth_dialog = Some(AuthDialog::Complete(match outcome {
                    OpenAiLoginResult::Installed { .. } => {
                        "ChatGPT credential installed. No model was selected. Coding integration is not enabled yet."
                    }
                    OpenAiLoginResult::CancelledBeforeInstallation => {
                        "Login cancelled before installation; existing credentials were not changed."
                    }
                    OpenAiLoginResult::Failed { failure } => failure_message(failure),
                }))
            }
            AuthEvent::Failed(message) => self.auth_dialog = Some(AuthDialog::Complete(message)),
        }
    }
}

pub(super) fn render(frame: &mut ratatui::Frame<'_>, dialog: &AuthDialog, scroll: &mut u16) {
    use crate::terminal::SafeText;
    use ratatui::{
        layout::Rect,
        widgets::{Block, Borders, Clear, Paragraph, Wrap},
    };
    let message=match dialog {
        AuthDialog::Choose {logout}=>format!("{} provider\n\n1 OpenCode (Zen/Go API key)\n2 ChatGPT subscription (browser)\n\nEsc cancel",if *logout {"Log out of"}else{"Log in to"}),
        AuthDialog::Loading {..}=>"Loading ChatGPT credential status…\n\nEsc cancel".into(),
        AuthDialog::Confirm {logout,status}=>format!("ChatGPT: {} · generation {}\n\n{}\n\nEnter confirm · Esc cancel",match status.state {
            OpenAiCredentialState::Unconfigured=>"not configured",OpenAiCredentialState::Configured=>"configured",OpenAiCredentialState::ReauthenticationRequired=>"new login required",
        },status.generation,if *logout {"Remove only the local ChatGPT credential? Remote authorization and dispatched requests may remain. This is not forensic erasure."}else{"Start browser login, replacing any current ChatGPT credential on success? Coding inference is not enabled yet. No other application's login is imported; no model will be selected."}),
        AuthDialog::Waiting {url:Some(url)}=>format!("Open this URL yourself in your browser. Do not paste the callback, code or tokens here.\n\n{}\n\nWaiting for authorization and durable installation…\nEsc cancel (a save already in progress may complete)",url.as_str()),
        AuthDialog::Waiting {url:None}=>"Starting ChatGPT login…\n\nEsc cancel".into(),
        AuthDialog::Removing=>"Removing the local ChatGPT credential…\n\nEsc waits for the outcome; removal cannot be rolled back.".into(),
        AuthDialog::Cancelling=>"Cancellation requested; waiting for controlled work. An installation/removal already started may still commit. Browser authorization may remain.".into(),
        AuthDialog::Complete(message)=>format!("{message}\n\nEnter/Esc close"),
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
    let safe = SafeText::from_untrusted(&message);
    let paragraph = Paragraph::new(safe.as_str()).wrap(Wrap { trim: false });
    let maximum = paragraph
        .line_count(popup.width.saturating_sub(2).max(1))
        .saturating_sub(usize::from(popup.height.saturating_sub(2)));
    *scroll = (*scroll).min(u16::try_from(maximum).unwrap_or(u16::MAX));
    frame.render_widget(Clear, popup);
    frame.render_widget(
        paragraph
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Auth · ↑/↓ scroll "),
            )
            .scroll((*scroll, 0)),
        popup,
    );
}
