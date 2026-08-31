mod conversions;

use std::{error::Error, fmt, sync::Arc};

use self::conversions::*;
use morons_protocol::{
    ApplicationError, ApplicationEvent, ApplicationRequest, ApplicationResponse, ResourceLimit,
    ServerEndpoint, SessionCatalogEventCursor as ProtocolSessionCatalogEventCursor,
};
use tokio::sync::watch;

use crate::{
    persistence::{PersistenceError, RunModelSelection, SessionCatalogEventCursor, SessionStore},
    provider::{
        OpenCodeModelAvailability, OpenCodeProvider, OpenCodeService, ProviderError,
        find_open_code_model,
    },
    run_supervisor::RunSupervisor,
};

const SESSION_CATALOG_REPLAY_PAGE_SIZE: u16 = 100;

pub struct ServerApplication {
    sessions: Arc<SessionStore>,
    open_code: Arc<OpenCodeProvider>,
    run_supervisor: Arc<RunSupervisor>,
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

    pub async fn shutdown(&self) {
        self.run_supervisor.shutdown().await;
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
            ApplicationRequest::SubmitSessionInput {
                mutation_request_id,
                session_id,
                text,
                service,
                model_id,
            } => {
                let mutation_request_id = to_persistence_mutation_id(mutation_request_id);
                let session_id = to_persistence_session_id(session_id);
                let persistence_service = to_persistence_service(service);
                if let Some(accepted) = self
                    .sessions
                    .find_session_input_retry(
                        mutation_request_id,
                        session_id,
                        &text,
                        persistence_service,
                        &model_id,
                    )
                    .await
                    .map_err(to_application_error)?
                {
                    return Ok(input_accepted_response(accepted));
                }

                let provider_service = to_provider_service(service);
                let model = find_open_code_model(provider_service, &model_id)
                    .ok_or(ApplicationError::UnsupportedModel)?;
                if self.run_supervisor.is_stopping() {
                    return Err(ApplicationError::ServiceUnavailable);
                }
                let permit = match self.run_supervisor.try_reserve() {
                    Some(permit) => permit,
                    None if self.run_supervisor.is_stopping() => {
                        return Err(ApplicationError::ServiceUnavailable);
                    }
                    None => {
                        return Err(ApplicationError::ResourceLimit {
                            resource: ResourceLimit::Runs,
                        });
                    }
                };
                let accepted = self
                    .sessions
                    .accept_session_input(
                        mutation_request_id,
                        session_id,
                        text,
                        RunModelSelection {
                            service: persistence_service,
                            model_id,
                            protocol_revision: model.responses_protocol_revision,
                            maximum_input_tokens: model.maximum_input_tokens,
                            maximum_output_tokens: model.maximum_output_tokens,
                        },
                    )
                    .await
                    .map_err(to_application_error)?;
                if accepted.newly_accepted {
                    let run_id = accepted.run.id;
                    if let Err(error) = self.run_supervisor.start(run_id, permit).await {
                        eprintln!("accepted run could not start: {error}");
                        self.sessions
                            .finish_run_stopped(run_id, None)
                            .await
                            .map_err(to_application_error)?;
                    }
                }
                Ok(input_accepted_response(accepted))
            }
            ApplicationRequest::GetRun { session_id, run_id } => {
                let run = self
                    .sessions
                    .get_run(
                        to_persistence_session_id(session_id),
                        to_persistence_run_id(run_id),
                    )
                    .await
                    .map_err(to_application_error)?
                    .ok_or(ApplicationError::RunNotFound)?;
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::RunFound {
                        run: to_run_summary(run),
                    },
                ))
            }
            ApplicationRequest::ListSessionTranscript {
                session_id,
                cursor,
                limit,
            } => {
                let page = self
                    .sessions
                    .list_session_transcript(
                        to_persistence_session_id(session_id),
                        cursor.map(to_persistence_transcript_cursor),
                        limit,
                    )
                    .await
                    .map_err(to_application_error)?;
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::SessionTranscriptListed {
                        entries: page
                            .entries
                            .into_iter()
                            .map(to_protocol_transcript_entry)
                            .collect(),
                        next_cursor: page.next_cursor.map(to_protocol_transcript_cursor),
                    },
                ))
            }
            ApplicationRequest::CancelRun {
                mutation_request_id,
                session_id,
                run_id,
            } => {
                let result = self
                    .sessions
                    .cancel_run(
                        to_persistence_mutation_id(mutation_request_id),
                        to_persistence_session_id(session_id),
                        to_persistence_run_id(run_id),
                    )
                    .await
                    .map_err(to_application_error)?;
                if result.intent_applied {
                    self.run_supervisor.signal_cancellation(result.run_id).await;
                }
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::RunCancellationResolved {
                        run_id: to_protocol_run_id(result.run_id),
                        state: to_protocol_run_state(result.state),
                        cancellation_requested: result.cancellation_requested,
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
        let open_code = Arc::new(OpenCodeProvider::new(Arc::clone(&sessions)));
        Self::from_shared_parts(sessions, open_code)
    }

    #[cfg(test)]
    pub(crate) fn from_session_store_for_test(sessions: SessionStore, base: &str) -> Self {
        let sessions = Arc::new(sessions);
        let open_code = Arc::new(OpenCodeProvider::for_test(Arc::clone(&sessions), base));
        Self::from_shared_parts(sessions, open_code)
    }

    fn from_shared_parts(sessions: Arc<SessionStore>, open_code: Arc<OpenCodeProvider>) -> Self {
        let run_supervisor = RunSupervisor::new(Arc::clone(&sessions), Arc::clone(&open_code));
        let (session_catalog_notifications, _) = watch::channel(0);
        Self {
            sessions,
            open_code,
            run_supervisor,
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
