use morons_protocol::MAX_OPENCODE_API_KEY_BYTES;
use ratatui_crossterm::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};

use super::{AppAction, AppState, CredentialDialog, InformationDialog, View};
use crate::terminal::CredentialBuffer;

const MAX_SESSION_NAME_BYTES: usize = 256;
const MOUSE_SCROLL_ROWS: usize = 3;

impl AppState {
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> AppAction {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return AppAction::None;
        }
        if let Some(dialog) = self.information_dialog {
            return match (dialog, key.code) {
                (InformationDialog::TrustNotice, KeyCode::Enter) => {
                    self.information_dialog = None;
                    self.set_status(
                        "Trusted-local mode acknowledged · press ? for safety and usage help",
                    );
                    AppAction::None
                }
                (InformationDialog::TrustNotice, KeyCode::Char('q') | KeyCode::Esc) => {
                    AppAction::Quit
                }
                (InformationDialog::Help, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('?')) => {
                    self.information_dialog = None;
                    AppAction::None
                }
                _ => AppAction::None,
            };
        }
        if self.model_dialog.is_some() {
            return self.handle_model_key(key.code, key.modifiers);
        }
        if key.code == KeyCode::Char('?') {
            self.information_dialog = Some(InformationDialog::Help);
            return AppAction::None;
        }
        if self.rename_dialog.is_some() {
            return self.handle_rename_key(key.code, key.modifiers);
        }
        if self.pending_unknown {
            return match key.code {
                KeyCode::Char('r') => AppAction::RetryPending,
                KeyCode::Char('a') | KeyCode::Esc => AppAction::AbandonPending,
                _ => AppAction::None,
            };
        }
        if let Some(session_id) = self.confirm_delete {
            return match key.code {
                KeyCode::Char('y') => {
                    self.confirm_delete = None;
                    AppAction::DeleteSession { session_id }
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.confirm_delete = None;
                    self.set_status("Session deletion cancelled");
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

    pub(crate) fn handle_mouse(&mut self, mouse: MouseEvent) -> AppAction {
        if self.view != View::Session
            || self.information_dialog.is_some()
            || self.model_dialog.is_some()
            || self.credential_dialog.is_some()
            || self.rename_dialog.is_some()
            || self.confirm_stop
            || self.confirm_delete.is_some()
        {
            return AppAction::None;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_transcript_lines_up(MOUSE_SCROLL_ROWS),
            MouseEventKind::ScrollDown => self.scroll_transcript_lines_down(MOUSE_SCROLL_ROWS),
            _ => AppAction::None,
        }
    }

    pub(crate) fn handle_paste(&mut self, paste: &str) {
        if self.model_dialog.is_some() {
            self.append_model_search(paste);
            return;
        }
        if let Some(input) = self.rename_dialog.as_mut() {
            input.push_paste(paste);
            if input.len_bytes() > MAX_SESSION_NAME_BYTES {
                self.set_status("Session names accept at most 256 UTF-8 bytes");
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
            && self.view == View::Session
            && self.pending.is_none()
            && !self.confirm_stop
            && self.confirm_delete.is_none()
        {
            self.prompt.push_paste(paste);
            self.reset_skill_completion();
        }
    }

    fn handle_model_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> AppAction {
        if code == KeyCode::Esc
            || (code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL))
        {
            self.model_dialog = None;
            self.set_status("Model selection cancelled");
            return AppAction::None;
        }
        match code {
            KeyCode::Up => {
                self.move_model_dialog_selection(true);
                AppAction::None
            }
            KeyCode::Down => {
                self.move_model_dialog_selection(false);
                AppAction::None
            }
            KeyCode::Backspace => {
                if let Some(dialog) = self.model_dialog.as_mut() {
                    dialog.query.backspace();
                    dialog.selected = 0;
                }
                AppAction::None
            }
            KeyCode::Enter => {
                let matches = self.model_dialog_matches();
                let selected = self
                    .model_dialog
                    .as_ref()
                    .map(|dialog| dialog.selected)
                    .unwrap_or_default();
                let selection = matches
                    .get(selected)
                    .and_then(|index| self.models.get(*index))
                    .map(|model| (model.model.service, model.model.id.clone()));
                let Some((service, model_id)) = selection else {
                    self.set_status("No available reviewed model is selected");
                    return AppAction::None;
                };
                self.model_dialog = None;
                AppAction::SetDefaultModel { service, model_id }
            }
            KeyCode::Char(character)
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.append_model_search(&character.to_string());
                AppAction::None
            }
            _ => AppAction::None,
        }
    }

    fn append_model_search(&mut self, value: &str) {
        let Some(dialog) = self.model_dialog.as_mut() else {
            return;
        };
        dialog.query.push_paste(value);
        let mut maximum = dialog.query.len_bytes().min(super::MAX_MODEL_SEARCH_BYTES);
        while !dialog.query.as_str().is_char_boundary(maximum) {
            maximum = maximum.saturating_sub(1);
        }
        let truncated = maximum < dialog.query.len_bytes();
        let _ = dialog.query.truncate(maximum);
        dialog.selected = 0;
        if truncated {
            self.set_status("Model search accepts at most 128 UTF-8 bytes");
        }
    }

    fn move_model_dialog_selection(&mut self, reverse: bool) {
        let count = self.model_dialog_matches().len();
        let Some(dialog) = self.model_dialog.as_mut() else {
            return;
        };
        if count == 0 {
            dialog.selected = 0;
        } else if reverse {
            dialog.selected = dialog.selected.checked_sub(1).unwrap_or(count - 1);
        } else {
            dialog.selected = (dialog.selected + 1) % count;
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

    fn handle_rename_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> AppAction {
        match code {
            KeyCode::Esc => {
                self.rename_dialog = None;
                self.set_status("Session rename cancelled");
                AppAction::None
            }
            KeyCode::Backspace => {
                if let Some(input) = self.rename_dialog.as_mut() {
                    input.backspace();
                }
                AppAction::None
            }
            KeyCode::Enter => {
                let Some(session_id) = self.selected_session_id() else {
                    self.rename_dialog = None;
                    return AppAction::None;
                };
                let Some(input) = self.rename_dialog.as_ref() else {
                    return AppAction::None;
                };
                let display_name = input.as_str().trim();
                if display_name.is_empty() || display_name.len() > MAX_SESSION_NAME_BYTES {
                    self.set_status("Enter a session name of at most 256 UTF-8 bytes");
                    return AppAction::None;
                }
                let display_name = display_name.to_owned();
                self.rename_dialog = None;
                AppAction::RenameSession {
                    session_id,
                    display_name,
                }
            }
            KeyCode::Char(character)
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if let Some(input) = self.rename_dialog.as_mut() {
                    let _ = input.push_character(character);
                    if input.len_bytes() > MAX_SESSION_NAME_BYTES {
                        self.set_status("Session names accept at most 256 UTF-8 bytes");
                    }
                }
                AppAction::None
            }
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
            KeyCode::Char('r') if self.pending.is_none() && !self.sessions.is_empty() => {
                self.rename_dialog = Some(crate::terminal::PromptBuffer::default());
                self.set_status("Enter a new name for the selected session");
                AppAction::None
            }
            KeyCode::Char('a') if self.pending.is_none() => self
                .sessions
                .get(self.selected_session)
                .map(|session| AppAction::SetSessionArchived {
                    session_id: session.summary.id,
                    archived: !session.summary.archived,
                })
                .unwrap_or(AppAction::None),
            KeyCode::Char('d') if self.pending.is_none() => {
                if self
                    .sessions
                    .get(self.selected_session)
                    .is_some_and(|session| session.summary.archived)
                {
                    self.confirm_delete = self.selected_session_id();
                    self.set_status(
                        "Confirm deleting Morons history and attachments; the working directory is never changed",
                    );
                } else {
                    self.set_status("Archive a session before deleting it");
                }
                AppAction::None
            }
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
            _ => AppAction::None,
        }
    }

    fn handle_session_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> AppAction {
        match code {
            KeyCode::Esc if self.pending.is_none() => AppAction::CloseSession,
            KeyCode::Tab => {
                let _ = self.complete_selected_skill();
                AppAction::None
            }
            KeyCode::BackTab => {
                let _ = self.cycle_skill_completion(true);
                AppAction::None
            }
            KeyCode::Up if self.cycle_skill_completion(true) => AppAction::None,
            KeyCode::Down if self.cycle_skill_completion(false) => AppAction::None,
            KeyCode::PageUp => self.scroll_transcript_page_up(),
            KeyCode::PageDown => self.scroll_transcript_page_down(),
            KeyCode::Home => self.scroll_transcript_to_start(),
            KeyCode::End => self.scroll_transcript_to_latest(),
            KeyCode::Backspace if self.pending.is_none() => {
                self.backspace_prompt();
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
        if self.transcript_page_loading {
            self.set_status("Wait for transcript history loading to finish");
            return AppAction::None;
        }
        if self.prompt.is_empty() {
            self.set_status("Enter a message before submitting");
            return AppAction::None;
        }
        let Some(session) = self.session.as_ref() else {
            return AppAction::None;
        };
        let prompt = self.prompt.as_str();
        if prompt == "/help" {
            self.prompt.clear();
            self.information_dialog = Some(InformationDialog::Help);
            return AppAction::None;
        }
        if let Some(search) = model_search(prompt).map(str::to_owned) {
            self.prompt.clear();
            self.open_model_dialog(&search);
            return AppAction::None;
        }
        if prompt == "/context" {
            let Some(model) = self.selected_model() else {
                self.set_status("No reviewed model is currently available");
                return AppAction::None;
            };
            return AppAction::ShowContext {
                session_id: session.summary.id,
                service: model.model.service,
                model_id: model.model.id.clone(),
            };
        }
        if session.is_historical_window() {
            self.set_status(
                "Press End to return to the latest transcript before starting new work",
            );
            return AppAction::None;
        }
        if session.summary.archived {
            self.set_status("Unarchive this session before starting new work");
            return AppAction::None;
        }
        if session.active_run_id.is_some() || session.active_command_id.is_some() {
            self.set_status("The selected session already has active work");
            return AppAction::None;
        }
        if !self.draft_images.is_empty()
            && (prompt.starts_with('!') || manual_compaction_guidance(prompt).is_some())
        {
            self.set_status(
                "Image attachments cannot be submitted with command or context controls",
            );
            return AppAction::None;
        }
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
        if prompt.starts_with("/compact") && manual_compaction_guidance(prompt).is_none() {
            self.set_status("Use /compact or /compact <instructions>");
            return AppAction::None;
        }
        let Some(model) = self.selected_model() else {
            self.set_status("No reviewed model is currently available");
            return AppAction::None;
        };
        if !self.draft_images.is_empty() && !model.model.capabilities.image_input {
            self.set_status("The selected model does not support image input; draft retained");
            return AppAction::None;
        }
        AppAction::SubmitInput {
            session_id: session.summary.id,
            text: self.prompt.as_str().to_owned(),
            attachments: self.image_uploads(),
            service: model.model.service,
            model_id: model.model.id.clone(),
        }
    }
}

fn model_search(prompt: &str) -> Option<&str> {
    if prompt == "/model" {
        return Some("");
    }
    prompt.strip_prefix("/model ").map(str::trim)
}

fn manual_compaction_guidance(prompt: &str) -> Option<Option<&str>> {
    if prompt == "/compact" {
        return Some(None);
    }
    prompt
        .strip_prefix("/compact ")
        .map(str::trim)
        .filter(|guidance| !guidance.is_empty())
        .map(Some)
}

fn credential_input_constraint() -> String {
    format!(
        "Credential input accepts at most {MAX_OPENCODE_API_KEY_BYTES} visible ASCII characters"
    )
}
