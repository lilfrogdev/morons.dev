use std::{error::Error, fmt};

use morons_protocol::{
    ApplicationError, ApplicationRequest, ApplicationResponse,
    MutationRequestId as ProtocolMutationRequestId, ResourceLimit, ServerEndpoint,
    SessionId as ProtocolSessionId, SessionListCursor as ProtocolSessionListCursor, SessionSummary,
};

use crate::persistence::{
    MutationRequestId, PersistenceError, PersistenceResourceLimit, Session, SessionId,
    SessionListCursor, SessionStore,
};

pub struct ServerApplication {
    sessions: SessionStore,
}

#[derive(Debug)]
pub struct ApplicationStartupError(PersistenceError);

impl fmt::Display for ApplicationStartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "server application initialization failed: {}",
            self.0
        )
    }
}

impl Error for ApplicationStartupError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.0)
    }
}

impl ServerApplication {
    pub fn open(server: &ServerEndpoint) -> Result<Self, ApplicationStartupError> {
        SessionStore::open(server)
            .map(Self::from_session_store)
            .map_err(ApplicationStartupError)
    }

    pub(crate) async fn execute_for_local_owner(
        &self,
        request: ApplicationRequest,
    ) -> Result<ApplicationResponse, ApplicationError> {
        match request {
            ApplicationRequest::CreateSession {
                mutation_request_id,
                display_name,
            } => self
                .sessions
                .create_session(
                    to_persistence_mutation_id(mutation_request_id),
                    display_name,
                )
                .await
                .map(|session| ApplicationResponse::SessionCreated {
                    session: to_session_summary(session),
                })
                .map_err(to_application_error),
            ApplicationRequest::GetSession { session_id } => {
                match self
                    .sessions
                    .get_session(to_persistence_session_id(session_id))
                    .await
                    .map_err(to_application_error)?
                {
                    Some(session) => Ok(ApplicationResponse::SessionFound {
                        session: to_session_summary(session),
                    }),
                    None => Err(ApplicationError::SessionNotFound),
                }
            }
            ApplicationRequest::ListSessions { cursor, limit } => {
                let page = self
                    .sessions
                    .list_sessions(cursor.map(to_persistence_cursor), limit)
                    .await
                    .map_err(to_application_error)?;
                Ok(ApplicationResponse::SessionsListed {
                    sessions: page.sessions.into_iter().map(to_session_summary).collect(),
                    next_cursor: page.next_cursor.map(to_protocol_cursor),
                })
            }
        }
    }

    pub(crate) const fn from_session_store(sessions: SessionStore) -> Self {
        Self { sessions }
    }
}

fn to_persistence_mutation_id(request_id: ProtocolMutationRequestId) -> MutationRequestId {
    MutationRequestId::from_bytes(*request_id.as_bytes())
}

fn to_persistence_session_id(session_id: ProtocolSessionId) -> SessionId {
    SessionId::from_bytes(*session_id.as_bytes())
}

fn to_persistence_cursor(cursor: ProtocolSessionListCursor) -> SessionListCursor {
    SessionListCursor::from_sequence(u64::from_be_bytes(*cursor.as_bytes()))
}

fn to_protocol_cursor(cursor: SessionListCursor) -> ProtocolSessionListCursor {
    ProtocolSessionListCursor::from_bytes(cursor.sequence().to_be_bytes())
}

fn to_session_summary(session: Session) -> SessionSummary {
    SessionSummary {
        id: ProtocolSessionId::from_bytes(*session.id.as_bytes()),
        display_name: session.display_name,
        created_at_milliseconds: session.created_at_milliseconds,
    }
}

fn to_application_error(error: PersistenceError) -> ApplicationError {
    if matches!(
        &error,
        PersistenceError::Io(_)
            | PersistenceError::Sqlite(_)
            | PersistenceError::Control(_)
            | PersistenceError::Randomness(_)
            | PersistenceError::InvalidState { .. }
            | PersistenceError::WorkerStopped
    ) {
        eprintln!("session application operation failed: {error}");
    }

    match error {
        PersistenceError::InvalidInput { .. } => ApplicationError::InvalidRequest,
        PersistenceError::RequestConflict => ApplicationError::RequestConflict,
        PersistenceError::ResourceLimit {
            resource: PersistenceResourceLimit::Sessions,
        } => ApplicationError::ResourceLimit {
            resource: ResourceLimit::Sessions,
        },
        PersistenceError::ResourceLimit {
            resource: PersistenceResourceLimit::LogicalSequence,
        } => ApplicationError::ResourceLimit {
            resource: ResourceLimit::Storage,
        },
        PersistenceError::WorkerStopped => ApplicationError::ServiceUnavailable,
        PersistenceError::Io(_)
        | PersistenceError::Sqlite(_)
        | PersistenceError::Control(_)
        | PersistenceError::Randomness(_)
        | PersistenceError::InvalidState { .. } => ApplicationError::Internal,
    }
}
