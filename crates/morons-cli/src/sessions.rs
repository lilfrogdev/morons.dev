use std::{error::Error, fmt};

use morons_protocol::{
    ApplicationError, ApplicationEvent, ApplicationRequest, ApplicationResponse, ClientMessage,
    FrameError, MutationRequestId, OpenCodeApiKey, OpenCodeCredentialStatus, ResourceLimit,
    ServerMessage, SessionCatalogEventCursor, SessionId, SessionListCursor, SessionSummary,
    read_server_message, write_client_message,
};
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPage {
    pub sessions: Vec<SessionSummary>,
    pub next_cursor: Option<SessionListCursor>,
    pub catalog_cursor: SessionCatalogEventCursor,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum ApplicationClientError {
    Frame(FrameError),
    ServerDisconnected,
    ConnectionUnusable,
    RequestIdentifierExhausted,
    ResponseIdentifierMismatch {
        expected_request_id: u64,
        received_request_id: u64,
    },
    UnexpectedServerMessage,
    UnexpectedApplicationResponse,
    SubscriptionCursorMismatch,
    EventCursorNotMonotonic,
    Application(ApplicationError),
}

impl fmt::Display for ApplicationClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(error) => write!(formatter, "application request frame failed: {error}"),
            Self::ServerDisconnected => {
                formatter.write_str("server disconnected during an application request")
            }
            Self::ConnectionUnusable => {
                formatter.write_str("application connection is no longer usable")
            }
            Self::RequestIdentifierExhausted => {
                formatter.write_str("connection request identifiers are exhausted")
            }
            Self::ResponseIdentifierMismatch {
                expected_request_id,
                received_request_id,
            } => write!(
                formatter,
                "server response identifier mismatch: expected {expected_request_id}, received {received_request_id}"
            ),
            Self::UnexpectedServerMessage => {
                formatter.write_str("server sent a message invalid for an application request")
            }
            Self::UnexpectedApplicationResponse => {
                formatter.write_str("server returned the wrong application response type")
            }
            Self::SubscriptionCursorMismatch => {
                formatter.write_str("server accepted a different session catalog cursor")
            }
            Self::EventCursorNotMonotonic => {
                formatter.write_str("server returned a non-monotonic session catalog cursor")
            }
            Self::Application(error) => write_application_error(formatter, *error),
        }
    }
}

impl Error for ApplicationClientError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Frame(error) => Some(error),
            Self::ServerDisconnected
            | Self::ConnectionUnusable
            | Self::RequestIdentifierExhausted
            | Self::ResponseIdentifierMismatch { .. }
            | Self::UnexpectedServerMessage
            | Self::UnexpectedApplicationResponse
            | Self::SubscriptionCursorMismatch
            | Self::EventCursorNotMonotonic
            | Self::Application(_) => None,
        }
    }
}

fn write_application_error(
    formatter: &mut fmt::Formatter<'_>,
    error: ApplicationError,
) -> fmt::Result {
    match error {
        ApplicationError::InvalidRequest => formatter.write_str("application request is invalid"),
        ApplicationError::RequestConflict => {
            formatter.write_str("mutation request identifier conflicts with prior input")
        }
        ApplicationError::SessionNotFound => formatter.write_str("session was not found"),
        ApplicationError::CredentialGenerationConflict => {
            formatter.write_str("OpenCode credential state changed")
        }
        ApplicationError::CredentialMutationNotApplied => {
            formatter.write_str("OpenCode credential update was not applied")
        }
        ApplicationError::ResourceLimit {
            resource: ResourceLimit::Sessions,
        } => formatter.write_str("session limit was reached"),
        ApplicationError::ResourceLimit {
            resource: ResourceLimit::Storage,
        } => formatter.write_str("session storage limit was reached"),
        ApplicationError::ServiceUnavailable => {
            formatter.write_str("application service is unavailable")
        }
        ApplicationError::Internal => formatter.write_str("application request failed internally"),
    }
}

impl From<FrameError> for ApplicationClientError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

pub struct ApplicationClient<S> {
    connection: S,
    next_request_id: u64,
    usable: bool,
}

impl<S> ApplicationClient<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    #[must_use]
    pub const fn from_negotiated_connection(connection: S) -> Self {
        Self {
            connection,
            next_request_id: 1,
            usable: true,
        }
    }

    pub async fn create_session(
        &mut self,
        mutation_request_id: MutationRequestId,
        display_name: Option<String>,
    ) -> Result<SessionSummary, ApplicationClientError> {
        let response = self
            .request(ApplicationRequest::CreateSession {
                mutation_request_id,
                display_name,
            })
            .await?;
        let ApplicationResponse::SessionCreated { session } = response else {
            return Err(self.unexpected_application_response());
        };
        Ok(session)
    }

    pub async fn open_code_credential_status(
        &mut self,
    ) -> Result<OpenCodeCredentialStatus, ApplicationClientError> {
        let response = self
            .request(ApplicationRequest::GetOpenCodeCredentialStatus)
            .await?;
        let ApplicationResponse::OpenCodeCredentialStatus { credential } = response else {
            return Err(self.unexpected_application_response());
        };
        Ok(credential)
    }

    pub async fn set_open_code_credential(
        &mut self,
        mutation_request_id: MutationRequestId,
        expected_generation: u64,
        api_key: OpenCodeApiKey,
    ) -> Result<OpenCodeCredentialStatus, ApplicationClientError> {
        let response = self
            .request(ApplicationRequest::SetOpenCodeCredential {
                mutation_request_id,
                expected_generation,
                api_key,
            })
            .await?;
        let ApplicationResponse::OpenCodeCredentialUpdated { credential } = response else {
            return Err(self.unexpected_application_response());
        };
        Ok(credential)
    }

    pub async fn remove_open_code_credential(
        &mut self,
        mutation_request_id: MutationRequestId,
        expected_generation: u64,
    ) -> Result<OpenCodeCredentialStatus, ApplicationClientError> {
        let response = self
            .request(ApplicationRequest::RemoveOpenCodeCredential {
                mutation_request_id,
                expected_generation,
            })
            .await?;
        let ApplicationResponse::OpenCodeCredentialUpdated { credential } = response else {
            return Err(self.unexpected_application_response());
        };
        Ok(credential)
    }

    pub async fn get_session(
        &mut self,
        session_id: SessionId,
    ) -> Result<Option<SessionSummary>, ApplicationClientError> {
        let response = match self
            .request(ApplicationRequest::GetSession { session_id })
            .await
        {
            Ok(response) => response,
            Err(ApplicationClientError::Application(ApplicationError::SessionNotFound)) => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let ApplicationResponse::SessionFound { session } = response else {
            return Err(self.unexpected_application_response());
        };
        Ok(Some(session))
    }

    pub async fn list_sessions(
        &mut self,
        cursor: Option<SessionListCursor>,
        limit: u16,
    ) -> Result<SessionPage, ApplicationClientError> {
        let response = self
            .request(ApplicationRequest::ListSessions { cursor, limit })
            .await?;
        let ApplicationResponse::SessionsListed {
            sessions,
            next_cursor,
            catalog_cursor,
        } = response
        else {
            return Err(self.unexpected_application_response());
        };
        Ok(SessionPage {
            sessions,
            next_cursor,
            catalog_cursor,
        })
    }

    pub async fn subscribe_to_session_catalog(
        mut self,
        cursor: SessionCatalogEventCursor,
    ) -> Result<SessionCatalogSubscription<S>, ApplicationClientError> {
        let response = self
            .request(ApplicationRequest::SubscribeSessionCatalog { cursor })
            .await?;
        let ApplicationResponse::SessionCatalogSubscriptionStarted {
            cursor: accepted_cursor,
        } = response
        else {
            return Err(self.unexpected_application_response());
        };
        if accepted_cursor != cursor {
            self.usable = false;
            return Err(ApplicationClientError::SubscriptionCursorMismatch);
        }
        Ok(SessionCatalogSubscription {
            connection: self.connection,
            cursor,
            usable: self.usable,
        })
    }

    #[must_use]
    pub fn into_inner(self) -> S {
        self.connection
    }

    async fn request(
        &mut self,
        request: ApplicationRequest,
    ) -> Result<ApplicationResponse, ApplicationClientError> {
        if !self.usable {
            return Err(ApplicationClientError::ConnectionUnusable);
        }
        let request_id = self.next_request_id;
        let Some(next_request_id) = request_id.checked_add(1) else {
            self.usable = false;
            return Err(ApplicationClientError::RequestIdentifierExhausted);
        };
        self.next_request_id = next_request_id;
        if let Err(error) = write_client_message(
            &mut self.connection,
            &ClientMessage::request(request_id, request),
        )
        .await
        {
            self.usable = false;
            return Err(ApplicationClientError::Frame(error));
        }

        let response = match read_server_message(&mut self.connection).await {
            Ok(Some(response)) => response,
            Ok(None) => {
                self.usable = false;
                return Err(ApplicationClientError::ServerDisconnected);
            }
            Err(error) => {
                self.usable = false;
                return Err(ApplicationClientError::Frame(error));
            }
        };
        match response {
            ServerMessage::Response {
                request_id: received_request_id,
                response,
            } if received_request_id == request_id => Ok(response),
            ServerMessage::RequestFailed {
                request_id: received_request_id,
                error,
            } if received_request_id == request_id => {
                Err(ApplicationClientError::Application(error))
            }
            ServerMessage::Response {
                request_id: received_request_id,
                ..
            }
            | ServerMessage::RequestFailed {
                request_id: received_request_id,
                ..
            } => {
                self.usable = false;
                Err(ApplicationClientError::ResponseIdentifierMismatch {
                    expected_request_id: request_id,
                    received_request_id,
                })
            }
            ServerMessage::Hello { .. }
            | ServerMessage::ProtocolVersionMismatch { .. }
            | ServerMessage::Event { .. }
            | ServerMessage::SubscriptionEnded { .. } => {
                self.usable = false;
                Err(ApplicationClientError::UnexpectedServerMessage)
            }
        }
    }

    fn unexpected_application_response(&mut self) -> ApplicationClientError {
        self.usable = false;
        ApplicationClientError::UnexpectedApplicationResponse
    }
}

pub struct SessionCatalogSubscription<S> {
    connection: S,
    cursor: SessionCatalogEventCursor,
    usable: bool,
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
                let next_cursor = event.cursor();
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

#[cfg(test)]
mod tests;
