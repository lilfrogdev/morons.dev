#[cfg(test)]
mod native_diagnostic_tests;
use morons_protocol::{
    ApplicationEvent, RunId, ServerMessage, SessionCatalogEventCursor, SessionEventCursor,
    SessionId, TranscriptEntry, read_server_message,
};
use tokio::io::{AsyncRead, AsyncWrite};

use super::{ApplicationClientError, valid_image_attachments, valid_session_summary};

pub struct SessionCatalogSubscription<S> {
    pub(super) connection: S,
    pub(super) cursor: SessionCatalogEventCursor,
    pub(super) usable: bool,
}

impl<S> SessionCatalogSubscription<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    pub async fn next_event(&mut self) -> Result<ApplicationEvent, ApplicationClientError> {
        if !self.usable {
            return Err(ApplicationClientError::ConnectionUnusable);
        }
        let message = match read_server_message(&mut self.connection).await {
            Ok(Some(message)) => message,
            Ok(None) => {
                self.usable = false;
                return Err(ApplicationClientError::ServerDisconnected);
            }
            Err(error) => {
                self.usable = false;
                return Err(ApplicationClientError::Frame(error));
            }
        };
        match message {
            ServerMessage::Event { event } => {
                let Some(next_cursor) = event.session_catalog_cursor() else {
                    self.usable = false;
                    return Err(ApplicationClientError::EventScopeMismatch);
                };
                if matches!(
                    &event,
                    ApplicationEvent::SessionCreated { session, .. }
                        | ApplicationEvent::SessionChanged { session, .. }
                        if !valid_session_summary(session)
                ) || matches!(
                    &event,
                    ApplicationEvent::SessionRemoved { session_id, .. }
                        if session_id.as_bytes().iter().all(|byte| *byte == 0)
                ) {
                    self.usable = false;
                    return Err(ApplicationClientError::EventScopeMismatch);
                }
                if next_cursor.as_bytes() <= self.cursor.as_bytes() {
                    self.usable = false;
                    return Err(ApplicationClientError::EventCursorNotMonotonic);
                }
                self.cursor = next_cursor;
                Ok(event)
            }
            ServerMessage::SubscriptionEnded { error } => {
                self.usable = false;
                Err(ApplicationClientError::Application(error))
            }
            ServerMessage::Hello { .. }
            | ServerMessage::ProtocolVersionMismatch { .. }
            | ServerMessage::Response { .. }
            | ServerMessage::OpenAiLoginFinished { .. }
            | ServerMessage::RequestFailed { .. } => {
                self.usable = false;
                Err(ApplicationClientError::UnexpectedServerMessage)
            }
        }
    }

    #[must_use]
    pub const fn cursor(&self) -> SessionCatalogEventCursor {
        self.cursor
    }
}

pub struct SessionSubscription<S> {
    pub(super) connection: S,
    pub(super) session_id: SessionId,
    pub(super) cursor: SessionEventCursor,
    pub(super) active_delta_run: Option<RunId>,
    pub(super) terminal_delta_run: Option<RunId>,
    pub(super) native_failure_run: Option<RunId>,
    pub(super) delta_sequence: u64,
    pub(super) usable: bool,
}

impl<S> SessionSubscription<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    pub async fn next_event(&mut self) -> Result<ApplicationEvent, ApplicationClientError> {
        if !self.usable {
            return Err(ApplicationClientError::ConnectionUnusable);
        }
        let message = match read_server_message(&mut self.connection).await {
            Ok(Some(message)) => message,
            Ok(None) => {
                self.usable = false;
                return Err(ApplicationClientError::ServerDisconnected);
            }
            Err(error) => {
                self.usable = false;
                return Err(ApplicationClientError::Frame(error));
            }
        };
        match message {
            ServerMessage::Event { event } => {
                self.validate_event(&event)?;
                Ok(event)
            }
            ServerMessage::SubscriptionEnded { error } => {
                self.usable = false;
                Err(ApplicationClientError::Application(error))
            }
            ServerMessage::Hello { .. }
            | ServerMessage::ProtocolVersionMismatch { .. }
            | ServerMessage::Response { .. }
            | ServerMessage::OpenAiLoginFinished { .. }
            | ServerMessage::RequestFailed { .. } => {
                self.usable = false;
                Err(ApplicationClientError::UnexpectedServerMessage)
            }
        }
    }

    #[must_use]
    pub const fn cursor(&self) -> SessionEventCursor {
        self.cursor
    }

    fn validate_event(&mut self, event: &ApplicationEvent) -> Result<(), ApplicationClientError> {
        match event {
            ApplicationEvent::SessionTranscriptEntryCommitted {
                cursor,
                session_id,
                entry,
            } => {
                if *session_id != self.session_id
                    || matches!(
                        entry,
                        TranscriptEntry::UserMessage {
                            text,
                            attachments,
                            ..
                        } if !valid_image_attachments(text, attachments)
                    )
                {
                    return Err(self.event_scope_mismatch());
                }
                self.advance_cursor(*cursor)
            }
            ApplicationEvent::SessionRunChanged { cursor, run } => {
                if run.session_id != self.session_id {
                    return Err(self.event_scope_mismatch());
                }
                self.advance_cursor(*cursor)?;
                if run.state.is_terminal() {
                    if self
                        .active_delta_run
                        .is_some_and(|active_run| active_run != run.id)
                    {
                        return Err(self.event_scope_mismatch());
                    }
                    if self.active_delta_run == Some(run.id) {
                        self.active_delta_run = None;
                        self.delta_sequence = 0;
                    }
                    if self.terminal_delta_run != Some(run.id) {
                        self.native_failure_run = (run.service
                            == morons_protocol::ModelService::OpenAiChatGpt
                            && run.state == morons_protocol::RunState::Failed
                            && run.failure
                                == Some(morons_protocol::RunFailureKind::ProviderProtocol))
                        .then_some(run.id);
                    }
                    self.terminal_delta_run = Some(run.id);
                } else {
                    self.native_failure_run = None;
                    if self.terminal_delta_run == Some(run.id) {
                        return Err(self.event_scope_mismatch());
                    }
                    match self.active_delta_run {
                        None => {
                            self.active_delta_run = Some(run.id);
                            self.terminal_delta_run = None;
                        }
                        Some(active_run) if active_run == run.id => {}
                        Some(_) => return Err(self.event_scope_mismatch()),
                    }
                }
                Ok(())
            }
            ApplicationEvent::SessionLocalCommandChanged {
                cursor, session_id, ..
            } => {
                if *session_id != self.session_id {
                    return Err(self.event_scope_mismatch());
                }
                self.advance_cursor(*cursor)
            }
            ApplicationEvent::SessionNativeResponseDiagnostic {
                session_id, run_id, ..
            } => {
                if *session_id != self.session_id
                    || self.native_failure_run != Some(*run_id)
                    || self.terminal_delta_run != Some(*run_id)
                    || self.active_delta_run.is_some()
                {
                    return Err(self.event_scope_mismatch());
                }
                self.native_failure_run = None;
                Ok(())
            }
            ApplicationEvent::SessionAssistantDelta {
                session_id,
                run_id,
                sequence,
                ..
            } => {
                if *session_id != self.session_id || self.terminal_delta_run == Some(*run_id) {
                    return Err(self.event_scope_mismatch());
                }
                match self.active_delta_run {
                    None => self.active_delta_run = Some(*run_id),
                    Some(active_run) if active_run == *run_id => {}
                    Some(_) => return Err(self.event_scope_mismatch()),
                }
                if *sequence <= self.delta_sequence {
                    self.usable = false;
                    return Err(ApplicationClientError::EventCursorNotMonotonic);
                }
                self.delta_sequence = *sequence;
                Ok(())
            }
            ApplicationEvent::SessionCreated { .. }
            | ApplicationEvent::SessionChanged { .. }
            | ApplicationEvent::SessionRemoved { .. } => Err(self.event_scope_mismatch()),
        }
    }

    fn advance_cursor(
        &mut self,
        next_cursor: SessionEventCursor,
    ) -> Result<(), ApplicationClientError> {
        if next_cursor.as_bytes()[..16] != self.session_id.as_bytes()[..] {
            return Err(self.event_scope_mismatch());
        }
        if next_cursor.as_bytes() <= self.cursor.as_bytes() {
            self.usable = false;
            return Err(ApplicationClientError::EventCursorNotMonotonic);
        }
        self.cursor = next_cursor;
        Ok(())
    }

    fn event_scope_mismatch(&mut self) -> ApplicationClientError {
        self.usable = false;
        ApplicationClientError::EventScopeMismatch
    }
}
