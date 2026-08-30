use std::{error::Error, fmt};

use morons_protocol::{
    ApplicationError, ApplicationRequest, ApplicationResponse, ClientMessage, FrameError,
    MutationRequestId, ResourceLimit, ServerMessage, SessionId, SessionListCursor, SessionSummary,
    read_server_message, write_client_message,
};
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPage {
    pub sessions: Vec<SessionSummary>,
    pub next_cursor: Option<SessionListCursor>,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum SessionClientError {
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
    Application(ApplicationError),
}

impl fmt::Display for SessionClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(error) => write!(formatter, "session request frame failed: {error}"),
            Self::ServerDisconnected => {
                formatter.write_str("server disconnected during a session request")
            }
            Self::ConnectionUnusable => {
                formatter.write_str("session connection is no longer usable")
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
                formatter.write_str("server sent a message invalid for a session request")
            }
            Self::UnexpectedApplicationResponse => {
                formatter.write_str("server returned the wrong session response type")
            }
            Self::Application(error) => write_application_error(formatter, *error),
        }
    }
}

impl Error for SessionClientError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Frame(error) => Some(error),
            Self::ServerDisconnected
            | Self::ConnectionUnusable
            | Self::RequestIdentifierExhausted
            | Self::ResponseIdentifierMismatch { .. }
            | Self::UnexpectedServerMessage
            | Self::UnexpectedApplicationResponse
            | Self::Application(_) => None,
        }
    }
}

fn write_application_error(
    formatter: &mut fmt::Formatter<'_>,
    error: ApplicationError,
) -> fmt::Result {
    match error {
        ApplicationError::InvalidRequest => formatter.write_str("session request is invalid"),
        ApplicationError::RequestConflict => {
            formatter.write_str("mutation request identifier conflicts with prior input")
        }
        ApplicationError::SessionNotFound => formatter.write_str("session was not found"),
        ApplicationError::ResourceLimit {
            resource: ResourceLimit::Sessions,
        } => formatter.write_str("session limit was reached"),
        ApplicationError::ResourceLimit {
            resource: ResourceLimit::Storage,
        } => formatter.write_str("session storage limit was reached"),
        ApplicationError::ServiceUnavailable => {
            formatter.write_str("session service is unavailable")
        }
        ApplicationError::Internal => formatter.write_str("session request failed internally"),
    }
}

impl From<FrameError> for SessionClientError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

pub struct SessionClient<S> {
    connection: S,
    next_request_id: u64,
    usable: bool,
}

impl<S> SessionClient<S>
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
    ) -> Result<SessionSummary, SessionClientError> {
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

    pub async fn get_session(
        &mut self,
        session_id: SessionId,
    ) -> Result<Option<SessionSummary>, SessionClientError> {
        let response = match self
            .request(ApplicationRequest::GetSession { session_id })
            .await
        {
            Ok(response) => response,
            Err(SessionClientError::Application(ApplicationError::SessionNotFound)) => {
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
    ) -> Result<SessionPage, SessionClientError> {
        let response = self
            .request(ApplicationRequest::ListSessions { cursor, limit })
            .await?;
        let ApplicationResponse::SessionsListed {
            sessions,
            next_cursor,
        } = response
        else {
            return Err(self.unexpected_application_response());
        };
        Ok(SessionPage {
            sessions,
            next_cursor,
        })
    }

    #[must_use]
    pub fn into_inner(self) -> S {
        self.connection
    }

    async fn request(
        &mut self,
        request: ApplicationRequest,
    ) -> Result<ApplicationResponse, SessionClientError> {
        if !self.usable {
            return Err(SessionClientError::ConnectionUnusable);
        }
        let request_id = self.next_request_id;
        let Some(next_request_id) = request_id.checked_add(1) else {
            self.usable = false;
            return Err(SessionClientError::RequestIdentifierExhausted);
        };
        self.next_request_id = next_request_id;
        if let Err(error) = write_client_message(
            &mut self.connection,
            &ClientMessage::request(request_id, request),
        )
        .await
        {
            self.usable = false;
            return Err(SessionClientError::Frame(error));
        }

        let response = match read_server_message(&mut self.connection).await {
            Ok(Some(response)) => response,
            Ok(None) => {
                self.usable = false;
                return Err(SessionClientError::ServerDisconnected);
            }
            Err(error) => {
                self.usable = false;
                return Err(SessionClientError::Frame(error));
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
            } if received_request_id == request_id => Err(SessionClientError::Application(error)),
            ServerMessage::Response {
                request_id: received_request_id,
                ..
            }
            | ServerMessage::RequestFailed {
                request_id: received_request_id,
                ..
            } => {
                self.usable = false;
                Err(SessionClientError::ResponseIdentifierMismatch {
                    expected_request_id: request_id,
                    received_request_id,
                })
            }
            ServerMessage::Hello { .. } | ServerMessage::ProtocolVersionMismatch { .. } => {
                self.usable = false;
                Err(SessionClientError::UnexpectedServerMessage)
            }
        }
    }

    fn unexpected_application_response(&mut self) -> SessionClientError {
        self.usable = false;
        SessionClientError::UnexpectedApplicationResponse
    }
}

#[cfg(test)]
mod tests {
    use morons_protocol::{
        ApplicationError, ApplicationRequest, ApplicationResponse, ClientMessage,
        MutationRequestId, ServerMessage, SessionId, SessionSummary, read_client_message,
        write_server_message,
    };

    use super::{SessionClient, SessionClientError};

    #[tokio::test(flavor = "current_thread")]
    async fn session_client_correlates_create_get_and_list_requests() {
        let (client_connection, mut server) = tokio::io::duplex(4096);
        let mut client = SessionClient::from_negotiated_connection(client_connection);
        let mutation_request_id = MutationRequestId::from_bytes([0x11; 16]);
        let session = SessionSummary {
            id: SessionId::from_bytes([0x22; 16]),
            display_name: Some("Client session".to_owned()),
            created_at_milliseconds: 42,
        };

        let client_exchange = async {
            let created = client
                .create_session(mutation_request_id, session.display_name.clone())
                .await
                .expect("client should create a session");
            assert_eq!(created, session);

            let found = client
                .get_session(session.id)
                .await
                .expect("client should get a session");
            assert_eq!(found, Some(session.clone()));

            let page = client
                .list_sessions(None, 10)
                .await
                .expect("client should list sessions");
            assert_eq!(page.sessions, vec![session.clone()]);
            assert_eq!(page.next_cursor, None);
        };
        let server_exchange = async {
            let create = read_request(&mut server, 1).await;
            assert_eq!(
                create,
                ApplicationRequest::CreateSession {
                    mutation_request_id,
                    display_name: Some("Client session".to_owned()),
                }
            );
            write_server_message(
                &mut server,
                &ServerMessage::response(
                    1,
                    ApplicationResponse::SessionCreated {
                        session: session.clone(),
                    },
                ),
            )
            .await
            .expect("create response should be written");

            assert_eq!(
                read_request(&mut server, 2).await,
                ApplicationRequest::GetSession {
                    session_id: session.id,
                }
            );
            write_server_message(
                &mut server,
                &ServerMessage::response(
                    2,
                    ApplicationResponse::SessionFound {
                        session: session.clone(),
                    },
                ),
            )
            .await
            .expect("get response should be written");

            assert_eq!(
                read_request(&mut server, 3).await,
                ApplicationRequest::ListSessions {
                    cursor: None,
                    limit: 10,
                }
            );
            write_server_message(
                &mut server,
                &ServerMessage::response(
                    3,
                    ApplicationResponse::SessionsListed {
                        sessions: vec![session.clone()],
                        next_cursor: None,
                    },
                ),
            )
            .await
            .expect("list response should be written");
        };

        tokio::join!(client_exchange, server_exchange);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn missing_session_is_returned_as_none() {
        let (client_connection, mut server) = tokio::io::duplex(1024);
        let mut client = SessionClient::from_negotiated_connection(client_connection);
        let session_id = SessionId::from_bytes([0x33; 16]);

        let client_exchange = async {
            assert_eq!(
                client
                    .get_session(session_id)
                    .await
                    .expect("not found should be a valid query result"),
                None
            );
        };
        let server_exchange = async {
            read_request(&mut server, 1).await;
            write_server_message(
                &mut server,
                &ServerMessage::request_failed(1, ApplicationError::SessionNotFound),
            )
            .await
            .expect("not-found response should be written");
        };

        tokio::join!(client_exchange, server_exchange);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mismatched_response_identifier_is_rejected() {
        let (client_connection, mut server) = tokio::io::duplex(1024);
        let mut client = SessionClient::from_negotiated_connection(client_connection);

        let client_exchange = async {
            let error = client
                .list_sessions(None, 10)
                .await
                .expect_err("mismatched response should fail");
            assert!(matches!(
                error,
                SessionClientError::ResponseIdentifierMismatch {
                    expected_request_id: 1,
                    received_request_id: 2,
                }
            ));
            assert!(matches!(
                client
                    .list_sessions(None, 10)
                    .await
                    .expect_err("protocol failure should poison the connection"),
                SessionClientError::ConnectionUnusable
            ));
        };
        let server_exchange = async {
            read_request(&mut server, 1).await;
            write_server_message(
                &mut server,
                &ServerMessage::response(
                    2,
                    ApplicationResponse::SessionsListed {
                        sessions: Vec::new(),
                        next_cursor: None,
                    },
                ),
            )
            .await
            .expect("mismatched response should be written");
        };

        tokio::join!(client_exchange, server_exchange);
    }

    async fn read_request<S>(connection: &mut S, expected_request_id: u64) -> ApplicationRequest
    where
        S: tokio::io::AsyncRead + Unpin,
    {
        let message = read_client_message(connection)
            .await
            .expect("client request should be readable")
            .expect("client should send a request");
        let ClientMessage::Request {
            request_id,
            request,
        } = message
        else {
            panic!("client sent an unexpected message");
        };
        assert_eq!(request_id, expected_request_id);
        request
    }
}
