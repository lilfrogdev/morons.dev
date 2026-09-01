use morons_protocol::{
    ApplicationEvent, RunId, ServerMessage, SessionCatalogEventCursor, SessionEventCursor,
    SessionId, read_server_message,
};
use tokio::io::{AsyncRead, AsyncWrite};

use super::{ApplicationClientError, valid_session_summary, valid_workspace_summary};

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
                        if !valid_session_summary(session)
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
                cursor, session_id, ..
            } => {
                if *session_id != self.session_id {
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
                    self.terminal_delta_run = Some(run.id);
                } else {
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
            ApplicationEvent::SessionWorkspaceChanged {
                cursor,
                session_id,
                workspace,
            } => {
                if *session_id != self.session_id || !valid_workspace_summary(*workspace) {
                    return Err(self.event_scope_mismatch());
                }
                self.advance_cursor(*cursor)
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
            ApplicationEvent::SessionCreated { .. } => Err(self.event_scope_mismatch()),
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
