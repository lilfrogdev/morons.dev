use std::{error::Error, fmt, sync::Arc};

use morons_protocol::{
    ApplicationError, ApplicationEvent, ApplicationRequest, ApplicationResponse,
    MutationRequestId as ProtocolMutationRequestId,
    OpenCodeCredentialStatus as ProtocolOpenCodeCredentialStatus, ResourceLimit, ServerEndpoint,
    SessionCatalogEventCursor as ProtocolSessionCatalogEventCursor, SessionId as ProtocolSessionId,
    SessionListCursor as ProtocolSessionListCursor, SessionSummary,
};
use tokio::sync::watch;

use crate::{
    persistence::{
        MutationRequestId, OpenCodeCredentialStatus, PersistenceError, PersistenceResourceLimit,
        Session, SessionCatalogEventCursor, SessionId, SessionListCursor, SessionStore,
    },
    provider::{OpenCodeModelAvailability, OpenCodeProvider, OpenCodeService, ProviderError},
};

const SESSION_CATALOG_REPLAY_PAGE_SIZE: u16 = 100;

pub struct ServerApplication {
    sessions: Arc<SessionStore>,
    open_code: OpenCodeProvider,
    session_catalog_notifications: watch::Sender<u64>,
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

pub(crate) enum ApplicationOutcome {
    Response(ApplicationResponse),
    SessionCatalogSubscription(SessionCatalogSubscription),
}

pub(crate) struct SessionCatalogSubscription {
    pub(crate) cursor: SessionCatalogEventCursor,
    pub(crate) notifications: watch::Receiver<u64>,
}

pub(crate) struct DeliveredSessionCatalogEvent {
    pub(crate) cursor: SessionCatalogEventCursor,
    pub(crate) event: ApplicationEvent,
}

impl SessionCatalogSubscription {
    pub(crate) fn protocol_cursor(&self) -> ProtocolSessionCatalogEventCursor {
        to_protocol_catalog_cursor(self.cursor)
    }

    pub(crate) const fn sequence(&self) -> u64 {
        self.cursor.sequence()
    }

    pub(crate) fn advance(&mut self, cursor: SessionCatalogEventCursor) {
        self.cursor = cursor;
    }
}

impl ServerApplication {
    pub fn open(server: &ServerEndpoint) -> Result<Self, ApplicationStartupError> {
        SessionStore::open(server)
            .map(Self::from_session_store)
            .map_err(ApplicationStartupError)
    }

    pub async fn open_code_model_availability(
        &self,
        service: OpenCodeService,
    ) -> Result<Vec<OpenCodeModelAvailability>, ProviderError> {
        self.open_code.fetch_catalog(service).await
    }

    pub(crate) async fn execute_for_local_owner(
        &self,
        request: ApplicationRequest,
    ) -> Result<ApplicationOutcome, ApplicationError> {
        match request {
            ApplicationRequest::CreateSession {
                mutation_request_id,
                display_name,
            } => {
                let session = self
                    .sessions
                    .create_session(
                        to_persistence_mutation_id(mutation_request_id),
                        display_name,
                    )
                    .await
                    .map_err(to_application_error)?;
                self.publish_session_catalog_event(session.updated_sequence);
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::SessionCreated {
                        session: to_session_summary(session),
                    },
                ))
            }
            ApplicationRequest::GetSession { session_id } => {
                match self
                    .sessions
                    .get_session(to_persistence_session_id(session_id))
                    .await
                    .map_err(to_application_error)?
                {
                    Some(session) => Ok(ApplicationOutcome::Response(
                        ApplicationResponse::SessionFound {
                            session: to_session_summary(session),
                        },
                    )),
                    None => Err(ApplicationError::SessionNotFound),
                }
            }
            ApplicationRequest::ListSessions { cursor, limit } => {
                let page = self
                    .sessions
                    .list_sessions(cursor.map(to_persistence_list_cursor), limit)
                    .await
                    .map_err(to_application_error)?;
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::SessionsListed {
                        sessions: page.sessions.into_iter().map(to_session_summary).collect(),
                        next_cursor: page.next_cursor.map(to_protocol_list_cursor),
                        catalog_cursor: to_protocol_catalog_cursor(page.catalog_cursor),
                    },
                ))
            }
            ApplicationRequest::SubscribeSessionCatalog { cursor } => {
                let cursor = to_persistence_catalog_cursor(cursor);
                let notifications = self.session_catalog_notifications.subscribe();
                self.sessions
                    .read_session_catalog_events(cursor, 1)
                    .await
                    .map_err(to_application_error)?;
                Ok(ApplicationOutcome::SessionCatalogSubscription(
                    SessionCatalogSubscription {
                        cursor,
                        notifications,
                    },
                ))
            }
            ApplicationRequest::GetOpenCodeCredentialStatus => {
                let credential = self
                    .sessions
                    .open_code_credential_status()
                    .await
                    .map_err(to_application_error)?;
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::OpenCodeCredentialStatus {
                        credential: to_protocol_credential_status(credential),
                    },
                ))
            }
            ApplicationRequest::SetOpenCodeCredential {
                mutation_request_id,
                expected_generation,
                api_key,
            } => {
                let credential = self
                    .sessions
                    .set_open_code_credential(
                        to_persistence_mutation_id(mutation_request_id),
                        expected_generation,
                        api_key.into_bytes(),
                    )
                    .await
                    .map_err(to_application_error)?;
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::OpenCodeCredentialUpdated {
                        credential: to_protocol_credential_status(credential),
                    },
                ))
            }
            ApplicationRequest::RemoveOpenCodeCredential {
                mutation_request_id,
                expected_generation,
            } => {
                let credential = self
                    .sessions
                    .remove_open_code_credential(
                        to_persistence_mutation_id(mutation_request_id),
                        expected_generation,
                    )
                    .await
                    .map_err(to_application_error)?;
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::OpenCodeCredentialUpdated {
                        credential: to_protocol_credential_status(credential),
                    },
                ))
            }
        }
    }

    pub(crate) async fn read_session_catalog_events(
        &self,
        cursor: SessionCatalogEventCursor,
    ) -> Result<Vec<DeliveredSessionCatalogEvent>, ApplicationError> {
        let page = self
            .sessions
            .read_session_catalog_events(cursor, SESSION_CATALOG_REPLAY_PAGE_SIZE)
            .await
            .map_err(to_application_error)?;
        Ok(page
            .events
            .into_iter()
            .map(|event| DeliveredSessionCatalogEvent {
                cursor: event.cursor,
                event: ApplicationEvent::SessionCreated {
                    cursor: to_protocol_catalog_cursor(event.cursor),
                    session: to_session_summary(event.session),
                },
            })
            .collect())
    }

    pub(crate) fn from_session_store(sessions: SessionStore) -> Self {
        let sessions = Arc::new(sessions);
        let open_code = OpenCodeProvider::new(Arc::clone(&sessions));
        let (session_catalog_notifications, _) = watch::channel(0);
        Self {
            sessions,
            open_code,
            session_catalog_notifications,
        }
    }

    fn publish_session_catalog_event(&self, event_sequence: u64) {
        self.session_catalog_notifications
            .send_if_modified(|current| {
                if event_sequence > *current {
                    *current = event_sequence;
                    true
                } else {
                    false
                }
            });
    }
}

fn to_persistence_mutation_id(request_id: ProtocolMutationRequestId) -> MutationRequestId {
    MutationRequestId::from_bytes(*request_id.as_bytes())
}

fn to_persistence_session_id(session_id: ProtocolSessionId) -> SessionId {
    SessionId::from_bytes(*session_id.as_bytes())
}

fn to_persistence_list_cursor(cursor: ProtocolSessionListCursor) -> SessionListCursor {
    let bytes = cursor.as_bytes();
    let mut snapshot_event_sequence = [0_u8; 8];
    snapshot_event_sequence.copy_from_slice(&bytes[..8]);
    let mut after_created_sequence = [0_u8; 8];
    after_created_sequence.copy_from_slice(&bytes[8..]);
    SessionListCursor::new(
        u64::from_be_bytes(snapshot_event_sequence),
        u64::from_be_bytes(after_created_sequence),
    )
}

fn to_protocol_list_cursor(cursor: SessionListCursor) -> ProtocolSessionListCursor {
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&cursor.snapshot_event_sequence().to_be_bytes());
    bytes[8..].copy_from_slice(&cursor.after_created_sequence().to_be_bytes());
    ProtocolSessionListCursor::from_bytes(bytes)
}

fn to_persistence_catalog_cursor(
    cursor: ProtocolSessionCatalogEventCursor,
) -> SessionCatalogEventCursor {
    SessionCatalogEventCursor::from_sequence(u64::from_be_bytes(*cursor.as_bytes()))
}

fn to_protocol_catalog_cursor(
    cursor: SessionCatalogEventCursor,
) -> ProtocolSessionCatalogEventCursor {
    ProtocolSessionCatalogEventCursor::from_bytes(cursor.sequence().to_be_bytes())
}

const fn to_protocol_credential_status(
    credential: OpenCodeCredentialStatus,
) -> ProtocolOpenCodeCredentialStatus {
    ProtocolOpenCodeCredentialStatus {
        configured: credential.configured,
        generation: credential.generation,
    }
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
        PersistenceError::CredentialGenerationConflict => {
            ApplicationError::CredentialGenerationConflict
        }
        PersistenceError::CredentialNotConfigured => ApplicationError::ServiceUnavailable,
        PersistenceError::CredentialMutationNotApplied => {
            ApplicationError::CredentialMutationNotApplied
        }
        PersistenceError::ResourceLimit {
            resource: PersistenceResourceLimit::Sessions,
        } => ApplicationError::ResourceLimit {
            resource: ResourceLimit::Sessions,
        },
        PersistenceError::ResourceLimit {
            resource:
                PersistenceResourceLimit::LogicalSequence
                | PersistenceResourceLimit::CredentialGeneration
                | PersistenceResourceLimit::CredentialMutations,
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
