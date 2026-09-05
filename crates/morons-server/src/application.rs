mod conversions;
pub(crate) mod events;

use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
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
use sha2::{Digest as _, Sha256};
use tokio::sync::{Mutex, watch};

use crate::{
    command_supervisor::CommandSupervisor,
    persistence::{
        DefaultModelSelection, PersistenceError, PreparedImageAttachment, RunModelSelection,
        SessionCatalogEventCursor, SessionCatalogEventKind, SessionEventCursor,
        SessionEventPayload, SessionId, SessionStore, TranscriptPageDirection,
    },
    provider::{
        OpenCodeModelAvailability, OpenCodeProvider, OpenCodeService, ProviderError,
        find_open_code_model,
    },
    run_supervisor::RunSupervisor,
    skills::SkillDiscovery,
};

const SESSION_CATALOG_REPLAY_PAGE_SIZE: u16 = 100;
const SESSION_REPLAY_PAGE_SIZE: u16 = 8;

pub struct ServerApplication {
    sessions: Arc<SessionStore>,
    open_code: Arc<OpenCodeProvider>,
    run_supervisor: Arc<RunSupervisor>,
    command_supervisor: Arc<CommandSupervisor>,
    session_event_hub: Arc<SessionEventHub>,
    skills: Arc<SkillDiscovery>,
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
    pub(crate) event: Option<ApplicationEvent>,
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
        self.command_supervisor.shutdown().await;
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
                let _lifecycle_guard = self.lifecycle_mutations.lock().await;
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
            ApplicationRequest::RenameSession {
                mutation_request_id,
                session_id,
                display_name,
            } => {
                let _lifecycle_guard = self.lifecycle_mutations.lock().await;
                let session = self
                    .sessions
                    .rename_session(
                        to_persistence_mutation_id(mutation_request_id),
                        to_persistence_session_id(session_id),
                        display_name,
                    )
                    .await
                    .map_err(to_application_error)?;
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::SessionRenamed {
                        session: to_session_summary(session),
                    },
                ))
            }
            ApplicationRequest::SetSessionArchived {
                mutation_request_id,
                session_id,
                archived,
            } => {
                let _lifecycle_guard = self.lifecycle_mutations.lock().await;
                let persistence_request_id = to_persistence_mutation_id(mutation_request_id);
                let persistence_session_id = to_persistence_session_id(session_id);
                let (session, already_applied) = self
                    .sessions
                    .prepare_session_archive(
                        persistence_request_id,
                        persistence_session_id,
                        archived,
                    )
                    .await
                    .map_err(to_application_error)?;
                let session = if already_applied {
                    session
                } else {
                    if archived {
                        let snapshot = self
                            .sessions
                            .list_session_transcript(persistence_session_id, None, 1)
                            .await
                            .map_err(to_application_error)?;
                        if let Some(run_id) = snapshot.active_run_id {
                            let cancellation = self
                                .sessions
                                .cancel_run(
                                    derived_lifecycle_mutation_id(mutation_request_id, b"run"),
                                    persistence_session_id,
                                    run_id,
                                )
                                .await
                                .map_err(to_application_error)?;
                            if cancellation.intent_applied {
                                self.run_supervisor.signal_cancellation(run_id).await;
                            }
                        }
                        if let Some(command_id) = snapshot.active_command_id {
                            let cancellation = self
                                .sessions
                                .cancel_local_command(
                                    derived_lifecycle_mutation_id(mutation_request_id, b"command"),
                                    persistence_session_id,
                                    command_id,
                                )
                                .await
                                .map_err(to_application_error)?;
                            if cancellation.intent_applied {
                                self.command_supervisor
                                    .signal_cancellation(command_id)
                                    .await;
                            }
                        }
                        tokio::time::timeout(Duration::from_secs(10), async {
                            loop {
                                let snapshot = self
                                    .sessions
                                    .list_session_transcript(persistence_session_id, None, 1)
                                    .await?;
                                if snapshot.active_run_id.is_none()
                                    && snapshot.active_command_id.is_none()
                                {
                                    return Ok::<_, PersistenceError>(());
                                }
                                tokio::time::sleep(Duration::from_millis(20)).await;
                            }
                        })
                        .await
                        .map_err(|_| ApplicationError::ServiceUnavailable)?
                        .map_err(to_application_error)?;
                        if !self
                            .run_supervisor
                            .terminate_session_runtime(persistence_session_id)
                            .await
                        {
                            return Err(ApplicationError::Internal);
                        }
                    }
                    self.sessions
                        .complete_session_archive(persistence_request_id)
                        .await
                        .map_err(to_application_error)?
                };
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::SessionArchiveChanged {
                        session: to_session_summary(session),
                    },
                ))
            }
            ApplicationRequest::DeleteSession {
                mutation_request_id,
                session_id,
            } => {
                let _lifecycle_guard = self.lifecycle_mutations.lock().await;
                let persistence_request_id = to_persistence_mutation_id(mutation_request_id);
                let persistence_session_id = to_persistence_session_id(session_id);
                let complete = self
                    .sessions
                    .prepare_session_delete(persistence_request_id, persistence_session_id)
                    .await
                    .map_err(to_application_error)?;
                if !complete {
                    if !self
                        .run_supervisor
                        .terminate_session_runtime(persistence_session_id)
                        .await
                    {
                        return Err(ApplicationError::Internal);
                    }
                    self.sessions
                        .clean_session_database(persistence_request_id)
                        .await
                        .map_err(to_application_error)?;
                    let deleted_session_id = self
                        .sessions
                        .complete_session_delete(persistence_request_id)
                        .await
                        .map_err(to_application_error)?;
                    if deleted_session_id != persistence_session_id {
                        return Err(ApplicationError::Internal);
                    }
                }
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::SessionDeleted { session_id },
                ))
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
            ApplicationRequest::GetDefaultOpenCodeModel => {
                let selection = self
                    .sessions
                    .default_model()
                    .await
                    .map_err(to_application_error)?
                    .map(to_protocol_model_selection);
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::DefaultOpenCodeModel { selection },
                ))
            }
            ApplicationRequest::SetDefaultOpenCodeModel {
                mutation_request_id,
                service,
                model_id,
            } => {
                find_open_code_model(to_provider_service(service), &model_id)
                    .ok_or(ApplicationError::UnsupportedModel)?;
                let selection = self
                    .sessions
                    .set_default_model(
                        to_persistence_mutation_id(mutation_request_id),
                        DefaultModelSelection {
                            service: to_persistence_service(service),
                            model_id,
                        },
                    )
                    .await
                    .map_err(to_application_error)?;
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::DefaultOpenCodeModelUpdated {
                        selection: to_protocol_model_selection(selection),
                    },
                ))
            }
            ApplicationRequest::GetApplicationSettings => {
                let subagent_model = self
                    .sessions
                    .subagent_model_setting()
                    .await
                    .map_err(to_application_error)?;
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::ApplicationSettings {
                        settings: morons_protocol::ApplicationSettings {
                            subagent_model: to_protocol_subagent_model_setting(subagent_model),
                        },
                    },
                ))
            }
            ApplicationRequest::SetSubagentModelSetting {
                mutation_request_id,
                setting,
            } => {
                let setting = to_persistence_subagent_model_setting(setting);
                if let crate::persistence::SubagentModelSetting::OpenCode { service, model_id } =
                    &setting
                {
                    let model = find_open_code_model(
                        to_provider_service(to_protocol_service(*service)),
                        model_id,
                    )
                    .ok_or(ApplicationError::UnsupportedModel)?;
                    if !model.capabilities.text_input
                        || !model.capabilities.text_output
                        || !model.capabilities.tool_calls
                    {
                        return Err(ApplicationError::UnsupportedModel);
                    }
                }
                let setting = self
                    .sessions
                    .set_subagent_model_setting(
                        to_persistence_mutation_id(mutation_request_id),
                        setting,
                    )
                    .await
                    .map_err(to_application_error)?;
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::ApplicationSettingsUpdated {
                        settings: morons_protocol::ApplicationSettings {
                            subagent_model: to_protocol_subagent_model_setting(setting),
                        },
                    },
                ))
            }
            ApplicationRequest::ListSessionSkills { session_id } => {
                let working_directory = self
                    .sessions
                    .get_session(to_persistence_session_id(session_id))
                    .await
                    .map_err(to_application_error)?
                    .ok_or(ApplicationError::SessionNotFound)?
                    .working_directory
                    .map(std::path::PathBuf::from)
                    .filter(|path| path.is_dir());
                let skills = Arc::clone(&self.skills);
                let catalog = tokio::task::spawn_blocking(move || {
                    skills.catalog(working_directory.as_deref())
                })
                .await
                .map_err(|_| ApplicationError::ServiceUnavailable)?;
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::SessionSkillsListed {
                        session_id,
                        skills: catalog
                            .skills
                            .into_iter()
                            .map(to_protocol_skill_summary)
                            .collect(),
                        warnings: catalog.warnings,
                    },
                ))
            }
            ApplicationRequest::GetSessionContext {
                session_id,
                service,
                model_id,
            } => {
                let model = find_open_code_model(to_provider_service(service), &model_id)
                    .ok_or(ApplicationError::UnsupportedModel)?;
                let status = self
                    .sessions
                    .session_context_status(
                        to_persistence_session_id(session_id),
                        model.maximum_input_tokens,
                        model.maximum_output_tokens,
                    )
                    .await
                    .map_err(to_application_error)?;
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::SessionContextFound {
                        context: morons_protocol::SessionContextStatus {
                            session_id,
                            service,
                            model_id,
                            context_policy_version: crate::persistence::CONTEXT_POLICY_VERSION,
                            estimated_input_tokens: status.estimated_input_tokens,
                            maximum_input_tokens: status.maximum_input_tokens,
                            maximum_output_tokens: status.maximum_output_tokens,
                            compaction_threshold_tokens: status.compaction_threshold_tokens,
                            checkpoint_source_entry_high_water: status
                                .checkpoint_source_entry_high_water,
                            checkpoint_estimated_summary_tokens: status
                                .checkpoint_estimated_summary_tokens,
                        },
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
                attachments,
                service,
                model_id,
            } => {
                let mutation_request_id = to_persistence_mutation_id(mutation_request_id);
                let session_id = to_persistence_session_id(session_id);
                let persistence_service = to_persistence_service(service);
                let prepared_attachments = prepare_image_uploads(attachments).await?;
                if let Some(accepted) = self
                    .sessions
                    .find_session_input_retry_with_images(
                        mutation_request_id,
                        session_id,
                        &text,
                        persistence_service,
                        &model_id,
                        &prepared_attachments,
                    )
                    .await
                    .map_err(to_application_error)?
                {
                    return Ok(input_accepted_response(accepted));
                }

                let provider_service = to_provider_service(service);
                let model = find_open_code_model(provider_service, &model_id)
                    .ok_or(ApplicationError::UnsupportedModel)?;
                if !prepared_attachments.is_empty() && !model.capabilities.image_input {
                    return Err(ApplicationError::UnsupportedModel);
                }
                let working_directory = self
                    .sessions
                    .get_session(session_id)
                    .await
                    .map_err(to_application_error)?
                    .ok_or(ApplicationError::SessionNotFound)?
                    .working_directory
                    .ok_or(ApplicationError::WorkingDirectoryUnavailable)?;
                let skills = Arc::clone(&self.skills);
                let skill_prompt = text.clone();
                let skill_context = tokio::task::spawn_blocking(move || {
                    skills.context(std::path::Path::new(&working_directory), &skill_prompt)
                })
                .await
                .map_err(|_| ApplicationError::ServiceUnavailable)?;
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
                    .accept_session_input_with_skills(
                        mutation_request_id,
                        session_id,
                        text,
                        RunModelSelection {
                            service: persistence_service,
                            model_id,
                            protocol_revision: model.protocol_revision,
                            maximum_input_tokens: model.maximum_input_tokens,
                            maximum_output_tokens: model.maximum_output_tokens,
                            supports_tool_calls: model.capabilities.tool_calls,
                            supports_image_input: model.capabilities.image_input,
                        },
                        skill_context,
                        prepared_attachments,
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
            ApplicationRequest::ExecuteLocalCommand {
                mutation_request_id,
                session_id,
                command,
                context_visible,
            } => {
                let persistence_request_id = to_persistence_mutation_id(mutation_request_id);
                let persistence_session_id = to_persistence_session_id(session_id);
                if let Some(existing) = self
                    .sessions
                    .find_local_command_retry(
                        persistence_request_id,
                        persistence_session_id,
                        &command,
                        context_visible,
                    )
                    .await
                    .map_err(to_application_error)?
                {
                    return Ok(ApplicationOutcome::Response(
                        ApplicationResponse::LocalCommandAccepted {
                            command_id: morons_protocol::LocalCommandId::from_bytes(
                                *existing.id.as_bytes(),
                            ),
                        },
                    ));
                }
                let _lifecycle_guard = self.lifecycle_mutations.lock().await;
                if let Some(existing) = self
                    .sessions
                    .find_local_command_retry(
                        persistence_request_id,
                        persistence_session_id,
                        &command,
                        context_visible,
                    )
                    .await
                    .map_err(to_application_error)?
                {
                    return Ok(ApplicationOutcome::Response(
                        ApplicationResponse::LocalCommandAccepted {
                            command_id: morons_protocol::LocalCommandId::from_bytes(
                                *existing.id.as_bytes(),
                            ),
                        },
                    ));
                }
                if self.stopping.load(Ordering::Acquire) {
                    return Err(ApplicationError::ServiceUnavailable);
                }
                let permit = self.command_supervisor.try_reserve().ok_or(
                    ApplicationError::ResourceLimit {
                        resource: ResourceLimit::Runs,
                    },
                )?;
                let session_id = persistence_session_id;
                let accepted = self
                    .sessions
                    .accept_local_command(
                        persistence_request_id,
                        session_id,
                        command,
                        context_visible,
                    )
                    .await
                    .map_err(to_application_error)?;
                if accepted.newly_accepted {
                    let working_directory = self
                        .sessions
                        .get_session(session_id)
                        .await
                        .map_err(to_application_error)?
                        .and_then(|session| session.working_directory)
                        .ok_or(ApplicationError::WorkingDirectoryUnavailable)?;
                    if let Err(error) = self
                        .command_supervisor
                        .start(accepted.clone(), working_directory.into(), permit)
                        .await
                    {
                        eprintln!("accepted local command could not start: {error}");
                        self.sessions
                            .complete_local_command(
                                accepted.id,
                                crate::tools::ToolResult::error(
                                    crate::tools::ToolErrorKind::NotDispatched,
                                ),
                            )
                            .await
                            .map_err(to_application_error)?;
                    }
                }
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::LocalCommandAccepted {
                        command_id: morons_protocol::LocalCommandId::from_bytes(
                            *accepted.id.as_bytes(),
                        ),
                    },
                ))
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
                direction,
                limit,
            } => {
                let direction = match direction {
                    morons_protocol::TranscriptPageDirection::Older => {
                        TranscriptPageDirection::Older
                    }
                    morons_protocol::TranscriptPageDirection::Newer => {
                        TranscriptPageDirection::Newer
                    }
                };
                let page = self
                    .sessions
                    .list_session_transcript_window(
                        to_persistence_session_id(session_id),
                        cursor.map(to_persistence_transcript_cursor),
                        direction,
                        limit,
                    )
                    .await
                    .map_err(to_application_error)?;
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::SessionTranscriptListed {
                        session: to_session_summary(page.session),
                        entries: page
                            .entries
                            .into_iter()
                            .map(to_protocol_transcript_entry)
                            .collect(),
                        runs: page.runs.into_iter().map(to_run_summary).collect(),
                        active_run_id: page.active_run_id.map(to_protocol_run_id),
                        active_command_id: page
                            .active_command_id
                            .map(|id| morons_protocol::LocalCommandId::from_bytes(*id.as_bytes())),
                        older_cursor: page.older_cursor.map(to_protocol_transcript_cursor),
                        newer_cursor: page.newer_cursor.map(to_protocol_transcript_cursor),
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
            ApplicationRequest::CancelLocalCommand {
                mutation_request_id,
                session_id,
                command_id,
            } => {
                let result = self
                    .sessions
                    .cancel_local_command(
                        to_persistence_mutation_id(mutation_request_id),
                        to_persistence_session_id(session_id),
                        crate::persistence::LocalCommandId::from_bytes(*command_id.as_bytes()),
                    )
                    .await
                    .map_err(to_application_error)?;
                if result.intent_applied {
                    self.command_supervisor
                        .signal_cancellation(result.command_id)
                        .await;
                }
                Ok(ApplicationOutcome::Response(
                    ApplicationResponse::LocalCommandCancellationResolved {
                        command_id,
                        cancellation_requested: result.cancellation_requested,
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
                event: match event.kind {
                    SessionCatalogEventKind::Created(session) => ApplicationEvent::SessionCreated {
                        cursor: to_protocol_catalog_cursor(event.cursor),
                        session: to_session_summary(session),
                    },
                    SessionCatalogEventKind::Changed(session) => ApplicationEvent::SessionChanged {
                        cursor: to_protocol_catalog_cursor(event.cursor),
                        session: to_session_summary(session),
                    },
                    SessionCatalogEventKind::Removed(session_id) => {
                        ApplicationEvent::SessionRemoved {
                            cursor: to_protocol_catalog_cursor(event.cursor),
                            session_id: morons_protocol::SessionId::from_bytes(
                                *session_id.as_bytes(),
                            ),
                        }
                    }
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
                    event: Some(ApplicationEvent::SessionTranscriptEntryCommitted {
                        cursor: to_protocol_session_event_cursor(event.cursor),
                        session_id: morons_protocol::SessionId::from_bytes(*session_id.as_bytes()),
                        entry: to_protocol_transcript_entry(entry),
                    }),
                },
                SessionEventPayload::RunChanged(run) => DeliveredSessionEvent {
                    cursor: event.cursor,
                    event: Some(ApplicationEvent::SessionRunChanged {
                        cursor: to_protocol_session_event_cursor(event.cursor),
                        run: to_run_summary(run),
                    }),
                },
                SessionEventPayload::LocalCommandChanged { command_id, active } => {
                    DeliveredSessionEvent {
                        cursor: event.cursor,
                        event: Some(ApplicationEvent::SessionLocalCommandChanged {
                            cursor: to_protocol_session_event_cursor(event.cursor),
                            session_id: morons_protocol::SessionId::from_bytes(
                                *session_id.as_bytes(),
                            ),
                            command_id: morons_protocol::LocalCommandId::from_bytes(
                                *command_id.as_bytes(),
                            ),
                            active,
                        }),
                    }
                }
                SessionEventPayload::WorkspaceChanged(_) => DeliveredSessionEvent {
                    cursor: event.cursor,
                    event: None,
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

    #[cfg(test)]
    pub(crate) fn from_session_store_with_ipython_for_test(
        sessions: SessionStore,
        provider_base: &str,
    ) -> Self {
        let sessions = Arc::new(sessions);
        let open_code = Arc::new(OpenCodeProvider::for_test(
            Arc::clone(&sessions),
            provider_base,
        ));
        let session_event_hub = SessionEventHub::new();
        let run_supervisor = RunSupervisor::with_ipython_for_test(
            Arc::clone(&sessions),
            Arc::clone(&open_code),
            Arc::clone(&session_event_hub),
        );
        Self::from_supervised_parts(
            sessions,
            open_code,
            run_supervisor,
            session_event_hub,
            [0x7f; 16],
        )
    }

    #[cfg(test)]
    pub(crate) fn from_session_store_with_search_for_test(
        sessions: SessionStore,
        provider_base: &str,
        search_origin: String,
    ) -> Self {
        let sessions = Arc::new(sessions);
        let open_code = Arc::new(OpenCodeProvider::for_test(
            Arc::clone(&sessions),
            provider_base,
        ));
        let session_event_hub = SessionEventHub::new();
        let run_supervisor = RunSupervisor::for_test(
            Arc::clone(&sessions),
            Arc::clone(&open_code),
            Arc::clone(&session_event_hub),
            search_origin,
        );
        Self::from_supervised_parts(
            sessions,
            open_code,
            run_supervisor,
            session_event_hub,
            [0x7f; 16],
        )
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
        Self::from_supervised_parts(
            sessions,
            open_code,
            run_supervisor,
            session_event_hub,
            host_epoch,
        )
    }

    fn from_supervised_parts(
        sessions: Arc<SessionStore>,
        open_code: Arc<OpenCodeProvider>,
        run_supervisor: Arc<RunSupervisor>,
        session_event_hub: Arc<SessionEventHub>,
        host_epoch: [u8; 16],
    ) -> Self {
        let command_supervisor = CommandSupervisor::new(Arc::clone(&sessions));
        let shutdown_requests = run_supervisor.shutdown_requests();
        Self {
            sessions,
            open_code,
            run_supervisor,
            command_supervisor,
            session_event_hub,
            skills: Arc::new(application_skill_discovery()),
            host_epoch,
            stopping: AtomicBool::new(false),
            lifecycle_mutations: Mutex::new(()),
            shutdown_requests,
        }
    }
}

fn derived_lifecycle_mutation_id(
    request_id: morons_protocol::MutationRequestId,
    purpose: &[u8],
) -> crate::persistence::MutationRequestId {
    let mut digest = Sha256::new();
    digest.update(b"morons.dev/session-lifecycle-derived-mutation/v1\0");
    digest.update(request_id.as_bytes());
    digest.update((purpose.len() as u64).to_be_bytes());
    digest.update(purpose);
    let digest = digest.finalize();
    let mut identifier = [0_u8; 16];
    identifier.copy_from_slice(&digest[..16]);
    if identifier.iter().all(|byte| *byte == 0) {
        identifier[0] = 1;
    }
    crate::persistence::MutationRequestId::from_bytes(identifier)
}

async fn prepare_image_uploads(
    uploads: Vec<morons_protocol::ImageUpload>,
) -> Result<Vec<PreparedImageAttachment>, ApplicationError> {
    if uploads.len() > 4 {
        return Err(ApplicationError::InvalidRequest);
    }
    tokio::task::spawn_blocking(move || {
        uploads
            .into_iter()
            .map(|upload| {
                if !crate::persistence::images::valid_display_name(&upload.display_name) {
                    return Err(ApplicationError::InvalidRequest);
                }
                let raw = morons_image::decode_base64(&upload.data_base64)
                    .map_err(|_| ApplicationError::InvalidRequest)?;
                let normalized = morons_image::normalize_image(&raw)
                    .map_err(|_| ApplicationError::InvalidRequest)?;
                let digest = Sha256::digest(&normalized.bytes).into();
                Ok(PreparedImageAttachment {
                    display_name: upload.display_name,
                    marker_start: upload.marker_start,
                    media_type: normalized.media_type,
                    width: normalized.width,
                    height: normalized.height,
                    bytes: normalized.bytes,
                    digest,
                })
            })
            .collect()
    })
    .await
    .map_err(|_| ApplicationError::Internal)?
}

fn application_skill_discovery() -> SkillDiscovery {
    #[cfg(test)]
    {
        SkillDiscovery::for_test(Vec::new())
    }
    #[cfg(not(test))]
    {
        SkillDiscovery::new()
    }
}
