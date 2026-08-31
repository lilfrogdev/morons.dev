use ratatui_crossterm::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::{AppAction, AppState, View};

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
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.handle_control_key(key.code);
        }
        match self.view {
            View::Sessions => self.handle_sessions_key(key.code),
            View::Session => self.handle_session_key(key.code, key.modifiers),
        }
    }

    pub(crate) fn handle_paste(&mut self, paste: &str) {
        if self.view == View::Session && self.pending.is_none() && !self.confirm_stop {
            self.prompt.push_paste(paste);
        }
    }

    fn handle_control_key(&mut self, code: KeyCode) -> AppAction {
        match code {
            KeyCode::Char('l') => AppAction::Refresh,
            KeyCode::Char('s') if self.pending.is_none() => {
                self.confirm_stop = true;
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
