use morons_protocol::{MAX_OPENCODE_API_KEY_BYTES, WorkspaceBlockReason, WorkspaceState};
use ratatui_crossterm::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::{AppAction, AppState, CredentialDialog, ExecutionImageDialog, RepositoryDialog, View};
use crate::terminal::{CredentialBuffer, RepositoryPathBuffer};

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
        if self.execution_image_dialog.is_some() {
            return self.handle_execution_image_key(key.code, key.modifiers);
        }
        if self.repository_dialog.is_some() {
            return self.handle_repository_key(key.code, key.modifiers);
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
        if let Some(dialog) = self.execution_image_dialog.as_mut() {
            let input = match dialog {
                ExecutionImageDialog::Toolchain { input }
                | ExecutionImageDialog::Cargo { input, .. } => Some(input),
                ExecutionImageDialog::Confirm { .. } => None,
            };
            if input.is_some_and(|input| !input.push_paste(paste)) {
                self.set_status("Execution image paths must be bounded single-line text");
            }
            return;
        }
        if let Some(RepositoryDialog::Enter { input }) = self.repository_dialog.as_mut() {
            if !input.push_paste(paste) {
                self.set_status("Repository paths must be bounded single-line text");
            }
            return;
        }
        if let Some(CredentialDialog::Enter { input, .. }) = self.credential_dialog.as_mut() {
            if !input.push_paste(paste) {
                self.set_status(credential_input_constraint());
            }
            return;
        }
        if self.credential_dialog.is_none()
            && self.repository_dialog.is_none()
            && self.execution_image_dialog.is_none()
            && self.view == View::Session
            && self.pending.is_none()
            && !self.confirm_stop
            && !self.confirm_uncertainty
        {
            self.prompt.push_paste(paste);
        }
    }

    fn handle_control_key(&mut self, code: KeyCode) -> AppAction {
        match code {
            KeyCode::Char('k') if self.pending.is_none() => {
                self.open_credential_dialog();
                AppAction::None
            }
            KeyCode::Char('i') if self.pending.is_none() => {
                self.open_execution_image_dialog();
                AppAction::None
            }
            KeyCode::Char('l') => AppAction::Refresh,
            KeyCode::Char('o') if self.pending.is_none() && self.view == View::Session => {
                self.open_repository_dialog();
                AppAction::None
            }
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
                    session.active_run_id.map(|run_id| AppAction::CancelRun {
                        session_id: session.summary.id,
                        run_id,
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

    fn open_execution_image_dialog(&mut self) {
        self.execution_image_dialog = Some(ExecutionImageDialog::Toolchain {
            input: RepositoryPathBuffer::default(),
        });
        self.set_status(
            "Enter the absolute Rust toolchain root containing bin/cargo and bin/rustc",
        );
    }

    fn handle_execution_image_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> AppAction {
        if matches!(
            self.execution_image_dialog,
            Some(ExecutionImageDialog::Confirm { .. })
        ) {
            return match code {
                KeyCode::Char('y') => {
                    let Some(ExecutionImageDialog::Confirm {
                        toolchain_source_path,
                        cargo_source_path,
                    }) = self.execution_image_dialog.take()
                    else {
                        return AppAction::None;
                    };
                    AppAction::ProvisionExecutionImage {
                        toolchain_source_path,
                        cargo_source_path,
                    }
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.clear_execution_image_interaction();
                    self.set_status("Execution image provisioning cancelled");
                    AppAction::None
                }
                _ => AppAction::None,
            };
        }
        match code {
            KeyCode::Esc => {
                self.clear_execution_image_interaction();
                self.set_status("Execution image provisioning cancelled");
                AppAction::None
            }
            KeyCode::Backspace => {
                match self.execution_image_dialog.as_mut() {
                    Some(ExecutionImageDialog::Toolchain { input })
                    | Some(ExecutionImageDialog::Cargo { input, .. }) => input.backspace(),
                    Some(ExecutionImageDialog::Confirm { .. }) | None => {}
                }
                AppAction::None
            }
            KeyCode::Enter => {
                let Some(dialog) = self.execution_image_dialog.take() else {
                    return AppAction::None;
                };
                match dialog {
                    ExecutionImageDialog::Toolchain { mut input } => {
                        if input.is_empty() {
                            self.execution_image_dialog =
                                Some(ExecutionImageDialog::Toolchain { input });
                            self.set_status("Enter an absolute toolchain root before continuing");
                        } else {
                            self.execution_image_dialog = Some(ExecutionImageDialog::Cargo {
                                toolchain_source_path: input.take(),
                                input: RepositoryPathBuffer::default(),
                            });
                            self.set_status(
                                "Enter the absolute Cargo home whose registry seed will be copied",
                            );
                        }
                    }
                    ExecutionImageDialog::Cargo {
                        toolchain_source_path,
                        mut input,
                    } => {
                        if input.is_empty() {
                            self.execution_image_dialog = Some(ExecutionImageDialog::Cargo {
                                toolchain_source_path,
                                input,
                            });
                            self.set_status("Enter an absolute Cargo home before continuing");
                        } else {
                            self.execution_image_dialog = Some(ExecutionImageDialog::Confirm {
                                toolchain_source_path,
                                cargo_source_path: input.take(),
                            });
                        }
                    }
                    ExecutionImageDialog::Confirm { .. } => unreachable!(),
                }
                AppAction::None
            }
            KeyCode::Char(character)
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let accepted = match self.execution_image_dialog.as_mut() {
                    Some(ExecutionImageDialog::Toolchain { input })
                    | Some(ExecutionImageDialog::Cargo { input, .. }) => {
                        input.push_character(character)
                    }
                    Some(ExecutionImageDialog::Confirm { .. }) | None => false,
                };
                if !accepted {
                    self.set_status("Execution image paths must be bounded single-line text");
                }
                AppAction::None
            }
            _ => AppAction::None,
        }
    }

    fn open_repository_dialog(&mut self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        if session.workspace.state != WorkspaceState::Empty {
            self.set_status("The selected session workspace cannot accept a repository import");
            return;
        }
        if !session.entries.is_empty()
            || !session.runs.is_empty()
            || session.active_run_id.is_some()
        {
            self.set_status("Repository import requires a pristine session");
            return;
        }
        self.repository_dialog = Some(RepositoryDialog::Enter {
            input: RepositoryPathBuffer::default(),
        });
        self.set_status("Enter an absolute local repository path");
    }

    fn handle_repository_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> AppAction {
        if matches!(
            self.repository_dialog,
            Some(RepositoryDialog::Confirm { .. })
        ) {
            return match code {
                KeyCode::Char('y') => {
                    let Some(RepositoryDialog::Confirm { source_path }) =
                        self.repository_dialog.take()
                    else {
                        return AppAction::None;
                    };
                    let Some(session_id) = self.session.as_ref().map(|session| session.summary.id)
                    else {
                        return AppAction::None;
                    };
                    AppAction::ImportRepository {
                        session_id,
                        source_path,
                    }
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.clear_repository_interaction();
                    self.set_status("Repository import cancelled");
                    AppAction::None
                }
                _ => AppAction::None,
            };
        }
        match code {
            KeyCode::Esc => {
                self.clear_repository_interaction();
                self.set_status("Repository import cancelled");
                AppAction::None
            }
            KeyCode::Backspace => {
                if let Some(RepositoryDialog::Enter { input }) = self.repository_dialog.as_mut() {
                    input.backspace();
                }
                AppAction::None
            }
            KeyCode::Enter => {
                let Some(RepositoryDialog::Enter { input }) = self.repository_dialog.as_mut()
                else {
                    return AppAction::None;
                };
                if input.is_empty() {
                    self.set_status("Enter an absolute repository path before continuing");
                    return AppAction::None;
                }
                let source_path = input.take();
                self.repository_dialog = Some(RepositoryDialog::Confirm { source_path });
                AppAction::None
            }
            KeyCode::Char(character)
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let accepted = match self.repository_dialog.as_mut() {
                    Some(RepositoryDialog::Enter { input }) => input.push_character(character),
                    _ => false,
                };
                if !accepted {
                    self.set_status("Repository paths must be bounded single-line text");
                }
                AppAction::None
            }
            _ => AppAction::None,
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
                self.select_next_model(false);
                AppAction::None
            }
            KeyCode::BackTab => {
                self.select_next_model(true);
                AppAction::None
            }
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
                AppAction::None
            }
            KeyCode::Enter if modifiers.contains(KeyModifiers::SHIFT) && self.pending.is_none() => {
                let _ = self.prompt.push_character('\n');
                AppAction::None
            }
            KeyCode::Enter if self.pending.is_none() => self.submit_action(),
            KeyCode::Char(character)
                if self.pending.is_none()
                    && !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let _ = self.prompt.push_character(character);
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
        if session.active_run_id.is_some() {
            self.set_status("The selected session already has an active run");
            return AppAction::None;
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
