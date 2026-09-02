mod conversions;
pub(crate) mod events;

use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub(crate) use self::events::{SessionCatalogSubscription, SessionSubscription};
use self::{
    conversions::*,
    events::{AssistantDelta, SessionEventHub},
};
use morons_protocol::{
    ApplicationError, ApplicationEvent, ApplicationRequest, ApplicationResponse, ResourceLimit,
    ServerEndpoint,
};
use tokio::sync::{Mutex, watch};

use crate::{
    persistence::{
        PersistenceError, RunModelSelection, SessionCatalogEventCursor, SessionEventCursor,
        SessionEventPayload, SessionId, SessionStore,
    },
    provider::{
        OpenCodeModelAvailability, OpenCodeProvider, OpenCodeService, ProviderError,
        find_open_code_model,
    },
    run_supervisor::RunSupervisor,
};

const SESSION_CATALOG_REPLAY_PAGE_SIZE: u16 = 100;
const SESSION_REPLAY_PAGE_SIZE: u16 = 8;

pub struct ServerApplication {
    sessions: Arc<SessionStore>,
    open_code: Arc<OpenCodeProvider>,
    run_supervisor: Arc<RunSupervisor>,
    session_event_hub: Arc<SessionEventHub>,
    host_epoch: [u8; 16],
    stopping: AtomicBool,
    lifecycle_mutations: Mutex<()>,
    shutdown_requests: watch::Sender<bool>,
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
    SessionSubscription(SessionSubscription),
    StopServerAccepted { current_server_stopping: bool },
}

pub(crate) struct DeliveredSessionCatalogEvent {
    pub(crate) cursor: SessionCatalogEventCursor,
    pub(crate) event: ApplicationEvent,
}

pub(crate) struct DeliveredSessionEvent {
    pub(crate) cursor: SessionEventCursor,
    pub(crate) event: ApplicationEvent,
}

impl ServerApplication {
    pub fn open(server: &ServerEndpoint) -> Result<Self, ApplicationStartupError> {
        let host_epoch = *server.host_epoch().as_bytes();
        SessionStore::open(server)
            .map(|sessions| Self::from_session_store_with_epoch(sessions, host_epoch))
            .map_err(ApplicationStartupError)
    }

    pub async fn open_code_model_availability(
        &self,
        service: OpenCodeService,
    ) -> Result<Vec<OpenCodeModelAvailability>, ProviderError> {
        self.open_code.fetch_catalog(service).await
    }

    pub fn subscribe_shutdown_requests(&self) -> watch::Receiver<bool> {
        self.shutdown_requests.subscribe()
    }

    pub async fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        self.run_supervisor.shutdown().await;
        self.sessions.drain_workspace_operations().await;
    }

    pub(crate) async fn execute_for_local_owner(
        &self,
        request: ApplicationRequest,
    ) -> Result<ApplicationOutcome, ApplicationError> {
        match request {
            ApplicationRequest::CreateSession {
                mutation_request_id,
                display_name,
                working_directory,
            } => {
                let session = self
                    .sessions
                    .create_session_at(
                        to_persistence_mutation_id(mutation_request_id),
                        display_name,
                        working_directory,
                    )
                    .await
                    .map_err(to_application_error)?;
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
                let notifications = self.sessions.subscribe_event_notifications();
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
            ApplicationRequest::ListOpenCodeModels { service } => {
                let availability = self
                    .open_code_model_availability(to_provider_service(service))
                    .await
                    .map_err(|error| {
                        eprintln!("OpenCode model catalog query failed: {error}");
                        ApplicationError::ServiceUnavailable
                    })?;
                let models = availability
                    .into_iter()
                    .map(to_protocol_model_summary)
                    .collect::<Option<Vec<_>>>()
                    .ok_or(ApplicationError::Internal)?;
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::OpenCodeModelsListed { service, models },
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
            ApplicationRequest::ImportRepository {
                mutation_request_id,
                session_id,
                source_path,
            } => {
                let _lifecycle_guard = self.lifecycle_mutations.lock().await;
                if self.stopping.load(Ordering::Acquire) || self.run_supervisor.is_stopping() {
                    return Err(ApplicationError::ServiceUnavailable);
                }
                let workspace = self
                    .sessions
                    .import_repository(
                        to_persistence_mutation_id(mutation_request_id),
                        to_persistence_session_id(session_id),
                        source_path,
                    )
                    .await
                    .map_err(to_application_error)?;
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::RepositoryImported {
                        session_id,
                        workspace: to_protocol_workspace_summary(workspace),
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
                let lifecycle_guard = self.lifecycle_mutations.lock().await;
                if self.stopping.load(Ordering::Acquire) || self.run_supervisor.is_stopping() {
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
                            supports_tool_calls: model.capabilities.tool_calls,
                        },
                    )
                    .await
                    .map_err(to_application_error)?;
                drop(lifecycle_guard);
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
                        session: to_session_summary(page.session),
                        workspace: to_protocol_workspace_summary(page.workspace),
                        entries: page
                            .entries
                            .into_iter()
                            .map(to_protocol_transcript_entry)
                            .collect(),
                        runs: page.runs.into_iter().map(to_run_summary).collect(),
                        active_run_id: page.active_run_id.map(to_protocol_run_id),
                        next_cursor: page.next_cursor.map(to_protocol_transcript_cursor),
                        event_cursor: to_protocol_session_event_cursor(page.event_cursor),
                    },
                ))
            }
            ApplicationRequest::SubscribeSession { session_id, cursor } => {
                let session_id = to_persistence_session_id(session_id);
                let cursor = to_persistence_session_event_cursor(cursor);
                let notifications = self.sessions.subscribe_event_notifications();
                let assistant_deltas = self.session_event_hub.subscribe_assistant_deltas();
                self.sessions
                    .read_session_events(session_id, cursor, 1)
                    .await
                    .map_err(to_application_error)?;
                Ok(ApplicationOutcome::SessionSubscription(
                    SessionSubscription {
                        session_id,
                        cursor,
                        notifications,
                        assistant_deltas,
                        active_run: None,
                        terminal_run: None,
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
            ApplicationRequest::AcknowledgeToolUncertainty {
                mutation_request_id,
                session_id,
                run_id,
            } => {
                let acknowledgement = self
                    .sessions
                    .acknowledge_tool_uncertainty(
                        to_persistence_mutation_id(mutation_request_id),
                        to_persistence_session_id(session_id),
                        to_persistence_run_id(run_id),
                    )
                    .await
                    .map_err(to_application_error)?;
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::ToolUncertaintyAcknowledged {
                        session_id,
                        run_id,
                        workspace: to_protocol_workspace_summary(acknowledgement.workspace),
                    },
                ))
            }
            ApplicationRequest::StopServer {
                mutation_request_id,
            } => {
                let _lifecycle_guard = self.lifecycle_mutations.lock().await;
                let result = self
                    .sessions
                    .request_server_stop(
                        to_persistence_mutation_id(mutation_request_id),
                        self.host_epoch,
                    )
                    .await
                    .map_err(to_application_error)?;
                if result.signal_current_supervisor {
                    if result.accepted_host_epoch != self.host_epoch {
                        return Err(ApplicationError::Internal);
                    }
                    self.stopping.store(true, Ordering::Release);
                    self.shutdown_requests.send_replace(true);
                }
                Ok(ApplicationOutcome::StopServerAccepted {
                    current_server_stopping: result.accepted_host_epoch == self.host_epoch
                        && self.stopping.load(Ordering::Acquire),
                })
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

    pub(crate) async fn read_session_events(
        &self,
        session_id: SessionId,
        cursor: SessionEventCursor,
    ) -> Result<Vec<DeliveredSessionEvent>, ApplicationError> {
        let page = self
            .sessions
            .read_session_events(session_id, cursor, SESSION_REPLAY_PAGE_SIZE)
            .await
            .map_err(to_application_error)?;
        Ok(page
            .events
            .into_iter()
            .map(|event| match event.payload {
                SessionEventPayload::TranscriptEntry(entry) => DeliveredSessionEvent {
                    cursor: event.cursor,
                    event: ApplicationEvent::SessionTranscriptEntryCommitted {
                        cursor: to_protocol_session_event_cursor(event.cursor),
                        session_id: morons_protocol::SessionId::from_bytes(*session_id.as_bytes()),
                        entry: to_protocol_transcript_entry(entry),
                    },
                },
                SessionEventPayload::RunChanged(run) => DeliveredSessionEvent {
                    cursor: event.cursor,
                    event: ApplicationEvent::SessionRunChanged {
                        cursor: to_protocol_session_event_cursor(event.cursor),
                        run: to_run_summary(run),
                    },
                },
                SessionEventPayload::WorkspaceChanged(workspace) => DeliveredSessionEvent {
                    cursor: event.cursor,
                    event: ApplicationEvent::SessionWorkspaceChanged {
                        cursor: to_protocol_session_event_cursor(event.cursor),
                        session_id: morons_protocol::SessionId::from_bytes(*session_id.as_bytes()),
                        workspace: to_protocol_workspace_summary(workspace),
                    },
                },
            })
            .collect())
    }

    pub(crate) fn assistant_delta_event(delta: AssistantDelta) -> ApplicationEvent {
        ApplicationEvent::SessionAssistantDelta {
            session_id: morons_protocol::SessionId::from_bytes(*delta.session_id.as_bytes()),
            run_id: to_protocol_run_id(delta.run_id),
            sequence: delta.sequence,
            delta: delta.delta,
            refusal: delta.refusal,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_session_store(sessions: SessionStore) -> Self {
        Self::from_session_store_with_epoch(sessions, [0x7f; 16])
    }

    fn from_session_store_with_epoch(sessions: SessionStore, host_epoch: [u8; 16]) -> Self {
        let sessions = Arc::new(sessions);
        let open_code = Arc::new(OpenCodeProvider::new(Arc::clone(&sessions)));
        Self::from_shared_parts(sessions, open_code, host_epoch)
    }

    #[cfg(test)]
    pub(crate) fn from_session_store_for_test(sessions: SessionStore, base: &str) -> Self {
        let sessions = Arc::new(sessions);
        let open_code = Arc::new(OpenCodeProvider::for_test(Arc::clone(&sessions), base));
        Self::from_shared_parts(sessions, open_code, [0x7f; 16])
    }

    fn from_shared_parts(
        sessions: Arc<SessionStore>,
        open_code: Arc<OpenCodeProvider>,
        host_epoch: [u8; 16],
    ) -> Self {
        let session_event_hub = SessionEventHub::new();
        let run_supervisor = RunSupervisor::new(
            Arc::clone(&sessions),
            Arc::clone(&open_code),
            Arc::clone(&session_event_hub),
        );
        let (shutdown_requests, _) = watch::channel(false);
        Self {
            sessions,
            open_code,
            run_supervisor,
            session_event_hub,
            host_epoch,
            stopping: AtomicBool::new(false),
            lifecycle_mutations: Mutex::new(()),
            shutdown_requests,
        }
    }
}
