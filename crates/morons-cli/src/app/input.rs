use morons_protocol::{MAX_OPENCODE_API_KEY_BYTES, WorkspaceBlockReason};
use ratatui_crossterm::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::{AppAction, AppState, CredentialDialog, View};
use crate::terminal::CredentialBuffer;

impl AppState {
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> AppAction {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return AppAction::None;
        }
        if self.pending_unknown {
            return match key.code {
                KeyCode::Char('r') => AppAction::RetryPending,
                KeyCode::Char('a') | KeyCode::Esc => AppAction::AbandonPending,
                _ => AppAction::None,
            };
        }
        if self.confirm_uncertainty {
            return match key.code {
                KeyCode::Char('y') => {
                    self.confirm_uncertainty = false;
                    self.session
                        .as_ref()
                        .and_then(|session| {
                            session.workspace.blocked_run_id.map(|run_id| {
                                AppAction::AcknowledgeToolUncertainty {
                                    session_id: session.summary.id,
                                    run_id,
                                }
                            })
                        })
                        .unwrap_or(AppAction::None)
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.confirm_uncertainty = false;
                    self.set_status("Tool uncertainty acknowledgement cancelled");
                    AppAction::None
                }
                _ => AppAction::None,
            };
        }
        if self.confirm_stop {
            return match key.code {
                KeyCode::Char('y') => {
                    self.confirm_stop = false;
                    AppAction::StopServer
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.confirm_stop = false;
                    AppAction::None
                }
                _ => AppAction::None,
            };
        }
        if self.credential_dialog.is_some() {
            return self.handle_credential_key(key.code, key.modifiers);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.handle_control_key(key.code);
        }
        match self.view {
            View::Sessions => self.handle_sessions_key(key.code),
            View::Session => self.handle_session_key(key.code, key.modifiers),
        }
    }

    pub(crate) fn handle_paste(&mut self, paste: &str) {
        if let Some(CredentialDialog::Enter { input, .. }) = self.credential_dialog.as_mut() {
            if !input.push_paste(paste) {
                self.set_status(credential_input_constraint());
            }
            return;
        }
        if self.credential_dialog.is_none()
            && self.view == View::Session
            && self.pending.is_none()
            && !self.confirm_stop
            && !self.confirm_uncertainty
        {
            self.prompt.push_paste(paste);
            self.reset_skill_completion();
        }
    }

    fn handle_control_key(&mut self, code: KeyCode) -> AppAction {
        match code {
            KeyCode::Char('k') if self.pending.is_none() => {
                self.open_credential_dialog();
                AppAction::None
            }
            KeyCode::Char('l') => AppAction::Refresh,
            KeyCode::Char('s') if self.pending.is_none() => {
                self.confirm_stop = true;
                AppAction::None
            }
            KeyCode::Char('a') if self.pending.is_none() && self.view == View::Session => {
                let uncertain = self.session.as_ref().is_some_and(|session| {
                    session.workspace.block_reason
                        == Some(WorkspaceBlockReason::UncertainToolEffect)
                        && session.workspace.blocked_run_id.is_some()
                });
                if uncertain {
                    self.confirm_uncertainty = true;
                    self.set_status("Confirm parking the uncertain effect without resolving it");
                }
                AppAction::None
            }
            KeyCode::Char('x') if self.view == View::Session && self.pending.is_none() => self
                .session
                .as_ref()
                .and_then(|session| {
                    session
                        .active_run_id
                        .map(|run_id| AppAction::CancelRun {
                            session_id: session.summary.id,
                            run_id,
                        })
                        .or_else(|| {
                            session.active_command_id.map(|command_id| {
                                AppAction::CancelLocalCommand {
                                    session_id: session.summary.id,
                                    command_id,
                                }
                            })
                        })
                })
                .unwrap_or(AppAction::None),
            _ => AppAction::None,
        }
    }

    fn open_credential_dialog(&mut self) {
        match self.credential {
            Some(status) if status.configured => {
                self.credential_dialog = Some(CredentialDialog::ChooseAction);
                self.set_status("Choose whether to replace or remove the OpenCode credential");
            }
            Some(_) => {
                self.credential_dialog = Some(CredentialDialog::Enter {
                    replacing: false,
                    input: CredentialBuffer::default(),
                });
                self.set_status("Enter the OpenCode credential; input is hidden");
            }
            None => self.set_status("Credential status is still loading"),
        }
    }

    fn handle_credential_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> AppAction {
        if matches!(self.credential_dialog, Some(CredentialDialog::ChooseAction)) {
            return match code {
                KeyCode::Char('r') => {
                    self.credential_dialog = Some(CredentialDialog::Enter {
                        replacing: true,
                        input: CredentialBuffer::default(),
                    });
                    self.set_status("Enter the replacement credential; input is hidden");
                    AppAction::None
                }
                KeyCode::Char('d') => {
                    self.credential_dialog = Some(CredentialDialog::ConfirmRemove);
                    AppAction::None
                }
                KeyCode::Esc => {
                    self.clear_credential_interaction();
                    self.set_status("Credential operation cancelled");
                    AppAction::None
                }
                _ => AppAction::None,
            };
        }
        if matches!(
            self.credential_dialog,
            Some(CredentialDialog::ConfirmRemove)
        ) {
            return match code {
                KeyCode::Char('y') => self.remove_credential_action(),
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.clear_credential_interaction();
                    self.set_status("Credential removal cancelled");
                    AppAction::None
                }
                _ => AppAction::None,
            };
        }
        match code {
            KeyCode::Esc => {
                self.clear_credential_interaction();
                self.set_status("Credential entry cancelled and cleared");
                AppAction::None
            }
            KeyCode::Backspace => {
                if let Some(CredentialDialog::Enter { input, .. }) = self.credential_dialog.as_mut()
                {
                    input.backspace();
                }
                AppAction::None
            }
            KeyCode::Enter => self.set_credential_action(),
            KeyCode::Char(character)
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let accepted = match self.credential_dialog.as_mut() {
                    Some(CredentialDialog::Enter { input, .. }) => input.push_character(character),
                    _ => false,
                };
                if !accepted {
                    self.set_status(credential_input_constraint());
                }
                AppAction::None
            }
            _ => AppAction::None,
        }
    }

    fn set_credential_action(&mut self) -> AppAction {
        let Some(status) = self.credential else {
            self.clear_credential_interaction();
            self.set_status("Credential status changed; refresh before trying again");
            return AppAction::None;
        };
        let empty = matches!(
            self.credential_dialog.as_ref(),
            Some(CredentialDialog::Enter { input, .. }) if input.is_empty()
        );
        if empty {
            self.set_status("Enter an OpenCode credential before saving");
            return AppAction::None;
        }
        let Some(CredentialDialog::Enter { input, .. }) = self.credential_dialog.take() else {
            return AppAction::None;
        };
        let Ok(api_key) = input.into_api_key() else {
            self.set_status("The OpenCode credential is invalid and was cleared");
            return AppAction::None;
        };
        AppAction::SetCredential {
            expected_generation: status.generation,
            api_key,
        }
    }

    fn remove_credential_action(&mut self) -> AppAction {
        self.clear_credential_interaction();
        let Some(status) = self.credential.filter(|status| status.configured) else {
            self.set_status("The OpenCode credential is no longer configured");
            return AppAction::None;
        };
        AppAction::RemoveCredential {
            expected_generation: status.generation,
        }
    }

    fn handle_sessions_key(&mut self, code: KeyCode) -> AppAction {
        match code {
            KeyCode::Char('q') if self.pending.is_none() => AppAction::Quit,
            KeyCode::Char('n') if self.pending.is_none() => AppAction::CreateSession,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected_session = self.selected_session.saturating_sub(1);
                AppAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected_session =
                    (self.selected_session + 1).min(self.sessions.len().saturating_sub(1));
                AppAction::None
            }
            KeyCode::Enter => self
                .selected_session_id()
                .map_or(AppAction::None, AppAction::OpenSession),
            KeyCode::Tab => {
                self.select_next_model(false);
                AppAction::None
            }
            KeyCode::BackTab => {
                self.select_next_model(true);
                AppAction::None
            }
            _ => AppAction::None,
        }
    }

    fn handle_session_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> AppAction {
        match code {
            KeyCode::Esc if self.pending.is_none() => AppAction::CloseSession,
            KeyCode::Tab => {
                if !self.complete_selected_skill() {
                    self.select_next_model(false);
                }
                AppAction::None
            }
            KeyCode::BackTab => {
                if !self.cycle_skill_completion(true) {
                    self.select_next_model(true);
                }
                AppAction::None
            }
            KeyCode::Up if self.cycle_skill_completion(true) => AppAction::None,
            KeyCode::Down if self.cycle_skill_completion(false) => AppAction::None,
            KeyCode::PageUp => {
                self.transcript_scroll = self.transcript_scroll.saturating_add(10);
                AppAction::None
            }
            KeyCode::PageDown => {
                self.transcript_scroll = self.transcript_scroll.saturating_sub(10);
                AppAction::None
            }
            KeyCode::Backspace if self.pending.is_none() => {
                self.prompt.backspace();
                self.reset_skill_completion();
                AppAction::None
            }
            KeyCode::Enter if modifiers.contains(KeyModifiers::SHIFT) && self.pending.is_none() => {
                let _ = self.prompt.push_character('\n');
                self.reset_skill_completion();
                AppAction::None
            }
            KeyCode::Enter if self.pending.is_none() => self.submit_action(),
            KeyCode::Char(character)
                if self.pending.is_none()
                    && !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let _ = self.prompt.push_character(character);
                self.reset_skill_completion();
                AppAction::None
            }
            _ => AppAction::None,
        }
    }

    fn submit_action(&mut self) -> AppAction {
        if self.prompt.is_empty() {
            self.set_status("Enter a message before submitting");
            return AppAction::None;
        }
        let Some(session) = self.session.as_ref() else {
            return AppAction::None;
        };
        if session.active_run_id.is_some() || session.active_command_id.is_some() {
            self.set_status("The selected session already has active work");
            return AppAction::None;
        }
        let prompt = self.prompt.as_str();
        if let Some(command) = prompt.strip_prefix("!!") {
            let command = command.trim_start();
            if command.is_empty() {
                self.set_status("Enter a command after !!");
                return AppAction::None;
            }
            return AppAction::ExecuteLocalCommand {
                session_id: session.summary.id,
                command: command.to_owned(),
                context_visible: false,
            };
        }
        if let Some(command) = prompt.strip_prefix('!') {
            let command = command.trim_start();
            if command.is_empty() {
                self.set_status("Enter a command after !");
                return AppAction::None;
            }
            return AppAction::ExecuteLocalCommand {
                session_id: session.summary.id,
                command: command.to_owned(),
                context_visible: true,
            };
        }
        let Some(model) = self.selected_model() else {
            self.set_status("No reviewed model is currently available");
            return AppAction::None;
        };
        AppAction::SubmitInput {
            session_id: session.summary.id,
            text: self.prompt.as_str().to_owned(),
            service: model.model.service,
            model_id: model.model.id.clone(),
        }
    }

    fn select_next_model(&mut self, reverse: bool) {
        let available: Vec<usize> = self
            .models
            .iter()
            .enumerate()
            .filter_map(|(index, model)| model.model.available.then_some(index))
            .collect();
        if available.is_empty() {
            self.selected_model = None;
            return;
        }
        let position = self
            .selected_model
            .and_then(|selected| available.iter().position(|index| *index == selected))
            .unwrap_or(0);
        let next = if reverse {
            position.checked_sub(1).unwrap_or(available.len() - 1)
        } else {
            (position + 1) % available.len()
        };
        self.selected_model = Some(available[next]);
    }
}

fn credential_input_constraint() -> String {
    format!(
        "Credential input accepts at most {MAX_OPENCODE_API_KEY_BYTES} visible ASCII characters"
    )
}
