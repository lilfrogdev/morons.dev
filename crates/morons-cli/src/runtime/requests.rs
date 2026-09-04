use std::time::Duration;

use interprocess::local_socket::tokio::Stream;
use morons_protocol::{
    ApplicationError, ApplicationSettings, FrameError, LocalCommandId, MutationRequestId,
    OpenCodeApiKey, OpenCodeCredentialStatus, OpenCodeModelSelection, OpenCodeModelSummary,
    OpenCodeService, RunId, RunSummary, SessionCatalogEventCursor, SessionContextStatus,
    SessionEventCursor, SessionId, SessionSummary, SkillSummary, SubagentModelSetting,
    TranscriptCursor, TranscriptEntry, TranscriptPageDirection,
};
use tokio::{sync::mpsc, time};

use crate::{
    ApplicationClient, ApplicationClientError, LocalCommandCancellationResult,
    RunCancellationResult, ServerStopAcceptance, SessionInputAcceptance, connect_or_start,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(35);
const RECONNECT_DELAY: Duration = Duration::from_millis(150);
const MAX_REQUEST_ATTEMPTS: usize = 3;
const SESSION_PAGE_SIZE: u16 = 100;
const MAX_SESSION_PAGES: usize = 100;
const TRANSCRIPT_ENTRY_PAGE_SIZE: u16 = 1;
const TRANSCRIPT_WINDOW_ENTRIES: usize = 64;

type Client = ApplicationClient<Stream>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TranscriptWindowTarget {
    Latest,
    Oldest,
    Older(TranscriptCursor),
    Newer(TranscriptCursor),
}

impl TranscriptWindowTarget {
    const fn request(self) -> (Option<TranscriptCursor>, TranscriptPageDirection) {
        match self {
            Self::Latest => (None, TranscriptPageDirection::Older),
            Self::Oldest => (None, TranscriptPageDirection::Newer),
            Self::Older(cursor) => (Some(cursor), TranscriptPageDirection::Older),
            Self::Newer(cursor) => (Some(cursor), TranscriptPageDirection::Newer),
        }
    }
}

pub(super) enum RequestCommand {
    LoadSessions,
    LoadModels(OpenCodeService),
    LoadDefaultModel,
    LoadSettings,
    LoadCredentialStatus,
    LoadSession(SessionId),
    LoadTranscriptWindow {
        session_id: SessionId,
        target: TranscriptWindowTarget,
    },
    LoadContext {
        session_id: SessionId,
        service: OpenCodeService,
        model_id: String,
    },
    SetDefaultModel {
        mutation_request_id: MutationRequestId,
        service: OpenCodeService,
        model_id: String,
    },
    SetSubagentModel {
        mutation_request_id: MutationRequestId,
        setting: SubagentModelSetting,
    },
    CreateSession {
        mutation_request_id: MutationRequestId,
    },
    RenameSession {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        display_name: String,
    },
    SetSessionArchived {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        archived: bool,
    },
    DeleteSession {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
    },
    SubmitInput {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        text: String,
        attachments: Vec<morons_protocol::ImageUpload>,
        service: OpenCodeService,
        model_id: String,
    },
    ExecuteLocalCommand {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        command: String,
        context_visible: bool,
    },
    CancelRun {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        run_id: RunId,
    },
    CancelLocalCommand {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        command_id: LocalCommandId,
    },
    SetCredential {
        mutation_request_id: MutationRequestId,
        expected_generation: u64,
        api_key: OpenCodeApiKey,
    },
    RemoveCredential {
        mutation_request_id: MutationRequestId,
        expected_generation: u64,
    },
    StopServer {
        mutation_request_id: MutationRequestId,
    },
}

impl RequestCommand {
    pub(super) const fn mutation_request_id(&self) -> Option<MutationRequestId> {
        match self {
            Self::LoadSessions
            | Self::LoadModels(_)
            | Self::LoadDefaultModel
            | Self::LoadSettings
            | Self::LoadCredentialStatus
            | Self::LoadSession(_)
            | Self::LoadTranscriptWindow { .. }
            | Self::LoadContext { .. } => None,
            Self::SetDefaultModel {
                mutation_request_id,
                ..
            }
            | Self::SetSubagentModel {
                mutation_request_id,
                ..
            }
            | Self::CreateSession {
                mutation_request_id,
            }
            | Self::RenameSession {
                mutation_request_id,
                ..
            }
            | Self::SetSessionArchived {
                mutation_request_id,
                ..
            }
            | Self::DeleteSession {
                mutation_request_id,
                ..
            }
            | Self::SubmitInput {
                mutation_request_id,
                ..
            }
            | Self::ExecuteLocalCommand {
                mutation_request_id,
                ..
            }
            | Self::CancelRun {
                mutation_request_id,
                ..
            }
            | Self::CancelLocalCommand {
                mutation_request_id,
                ..
            }
            | Self::SetCredential {
                mutation_request_id,
                ..
            }
            | Self::RemoveCredential {
                mutation_request_id,
                ..
            }
            | Self::StopServer {
                mutation_request_id,
            } => Some(*mutation_request_id),
        }
    }

    const fn context(&self) -> &'static str {
        match self {
            Self::LoadSessions => "session list",
            Self::LoadModels(_) => "model list",
            Self::LoadDefaultModel => "default model",
            Self::LoadSettings => "application settings",
            Self::LoadCredentialStatus => "credential status",
            Self::LoadSession(_) => "session transcript and skills",
            Self::LoadTranscriptWindow { .. } => "transcript history page",
            Self::LoadContext { .. } => "session context status",
            Self::SetDefaultModel { .. } => "default model selection",
            Self::SetSubagentModel { .. } => "subagent model setting",
            Self::CreateSession { .. } => "session creation",
            Self::RenameSession { .. } => "session rename",
            Self::SetSessionArchived { .. } => "session archive change",
            Self::DeleteSession { .. } => "session deletion",
            Self::SubmitInput { .. } => "message submission",
            Self::ExecuteLocalCommand { .. } => "local command",
            Self::CancelRun { .. } => "run cancellation",
            Self::CancelLocalCommand { .. } => "local command cancellation",
            Self::SetCredential { .. } => "credential configuration",
            Self::RemoveCredential { .. } => "credential removal",
            Self::StopServer { .. } => "server stop",
        }
    }

    const fn is_credential_mutation(&self) -> bool {
        matches!(
            self,
            Self::SetCredential { .. } | Self::RemoveCredential { .. }
        )
    }

    pub(super) fn clone_for_retry(&self) -> Option<Self> {
        match self {
            Self::LoadSessions => Some(Self::LoadSessions),
            Self::LoadModels(service) => Some(Self::LoadModels(*service)),
            Self::LoadDefaultModel => Some(Self::LoadDefaultModel),
            Self::LoadSettings => Some(Self::LoadSettings),
            Self::LoadCredentialStatus => Some(Self::LoadCredentialStatus),
            Self::LoadSession(session_id) => Some(Self::LoadSession(*session_id)),
            Self::LoadTranscriptWindow { session_id, target } => Some(Self::LoadTranscriptWindow {
                session_id: *session_id,
                target: *target,
            }),
            Self::LoadContext {
                session_id,
                service,
                model_id,
            } => Some(Self::LoadContext {
                session_id: *session_id,
                service: *service,
                model_id: model_id.clone(),
            }),
            Self::SetDefaultModel {
                mutation_request_id,
                service,
                model_id,
            } => Some(Self::SetDefaultModel {
                mutation_request_id: *mutation_request_id,
                service: *service,
                model_id: model_id.clone(),
            }),
            Self::SetSubagentModel {
                mutation_request_id,
                setting,
            } => Some(Self::SetSubagentModel {
                mutation_request_id: *mutation_request_id,
                setting: setting.clone(),
            }),
            Self::CreateSession {
                mutation_request_id,
            } => Some(Self::CreateSession {
                mutation_request_id: *mutation_request_id,
            }),
            Self::RenameSession {
                mutation_request_id,
                session_id,
                display_name,
            } => Some(Self::RenameSession {
                mutation_request_id: *mutation_request_id,
                session_id: *session_id,
                display_name: display_name.clone(),
            }),
            Self::SetSessionArchived {
                mutation_request_id,
                session_id,
                archived,
            } => Some(Self::SetSessionArchived {
                mutation_request_id: *mutation_request_id,
                session_id: *session_id,
                archived: *archived,
            }),
            Self::DeleteSession {
                mutation_request_id,
                session_id,
            } => Some(Self::DeleteSession {
                mutation_request_id: *mutation_request_id,
                session_id: *session_id,
            }),
            Self::SubmitInput {
                mutation_request_id,
                session_id,
                text,
                attachments,
                service,
                model_id,
            } => Some(Self::SubmitInput {
                mutation_request_id: *mutation_request_id,
                session_id: *session_id,
                text: text.clone(),
                attachments: attachments.clone(),
                service: *service,
                model_id: model_id.clone(),
            }),
            Self::ExecuteLocalCommand {
                mutation_request_id,
                session_id,
                command,
                context_visible,
            } => Some(Self::ExecuteLocalCommand {
                mutation_request_id: *mutation_request_id,
                session_id: *session_id,
                command: command.clone(),
                context_visible: *context_visible,
            }),
            Self::CancelRun {
                mutation_request_id,
                session_id,
                run_id,
            } => Some(Self::CancelRun {
                mutation_request_id: *mutation_request_id,
                session_id: *session_id,
                run_id: *run_id,
            }),
            Self::CancelLocalCommand {
                mutation_request_id,
                session_id,
                command_id,
            } => Some(Self::CancelLocalCommand {
                mutation_request_id: *mutation_request_id,
                session_id: *session_id,
                command_id: *command_id,
            }),
            Self::StopServer {
                mutation_request_id,
            } => Some(Self::StopServer {
                mutation_request_id: *mutation_request_id,
            }),
            Self::SetCredential { .. } | Self::RemoveCredential { .. } => None,
        }
    }
}

pub(super) enum RequestEvent {
    ConnectionRestored {
        server_version: String,
    },
    SessionsLoaded {
        sessions: Vec<SessionSummary>,
        cursor: SessionCatalogEventCursor,
    },
    ModelsLoaded {
        service: OpenCodeService,
        models: Vec<OpenCodeModelSummary>,
    },
    DefaultModelLoaded(Option<OpenCodeModelSelection>),
    DefaultModelUpdated {
        mutation_request_id: MutationRequestId,
        selection: OpenCodeModelSelection,
    },
    SettingsLoaded(ApplicationSettings),
    SettingsUpdated {
        mutation_request_id: MutationRequestId,
        settings: ApplicationSettings,
    },
    CredentialStatusLoaded(OpenCodeCredentialStatus),
    SessionLoaded(SessionSnapshot),
    TranscriptWindowLoaded {
        target: TranscriptWindowTarget,
        window: TranscriptWindow,
    },
    ContextLoaded(SessionContextStatus),
    SessionCreated {
        mutation_request_id: MutationRequestId,
        session: SessionSummary,
    },
    SessionRenamed {
        mutation_request_id: MutationRequestId,
        session: SessionSummary,
    },
    SessionArchiveChanged {
        mutation_request_id: MutationRequestId,
        session: SessionSummary,
    },
    SessionDeleted {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
    },
    InputAccepted {
        mutation_request_id: MutationRequestId,
        accepted: SessionInputAcceptance,
    },
    LocalCommandAccepted {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        command_id: LocalCommandId,
    },
    CancellationResolved {
        mutation_request_id: MutationRequestId,
        result: RunCancellationResult,
    },
    LocalCommandCancellationResolved {
        mutation_request_id: MutationRequestId,
        command_id: LocalCommandId,
        cancellation_requested: bool,
    },
    CredentialUpdated {
        mutation_request_id: MutationRequestId,
        credential: OpenCodeCredentialStatus,
    },
    CredentialMutationFailed {
        mutation_request_id: MutationRequestId,
        context: &'static str,
        error: String,
        outcome_unknown: bool,
    },
    ServerStopAccepted {
        mutation_request_id: MutationRequestId,
        result: ServerStopAcceptance,
    },
    QueryFailed {
        context: &'static str,
        model_service: Option<OpenCodeService>,
        error: String,
    },
    MutationFailed {
        mutation_request_id: MutationRequestId,
        context: &'static str,
        error: String,
    },
    MutationOutcomeUnknown {
        mutation_request_id: MutationRequestId,
        context: &'static str,
        error: String,
    },
}

pub(super) struct TranscriptWindow {
    pub(super) session: SessionSummary,
    pub(super) entries: Vec<TranscriptEntry>,
    pub(super) runs: Vec<RunSummary>,
    pub(super) active_run_id: Option<RunId>,
    pub(super) active_command_id: Option<LocalCommandId>,
    pub(super) older_cursor: Option<TranscriptCursor>,
    pub(super) newer_cursor: Option<TranscriptCursor>,
    pub(super) event_cursor: SessionEventCursor,
}

pub(super) struct SessionSnapshot {
    pub(super) window: TranscriptWindow,
    pub(super) skills: Vec<SkillSummary>,
    pub(super) skill_warnings: Vec<String>,
}

pub(super) async fn run_request_worker(
    mut client: Client,
    mut commands: mpsc::Receiver<RequestCommand>,
    events: mpsc::Sender<RequestEvent>,
) {
    while let Some(command) = commands.recv().await {
        if command.is_credential_mutation() {
            if send_credential_result(&mut client, command, &events)
                .await
                .is_err()
            {
                return;
            }
            continue;
        }
        let mut last_error = None;
        let mut completed = false;
        for attempt in 0..MAX_REQUEST_ATTEMPTS {
            let result = time::timeout(REQUEST_TIMEOUT, execute(&mut client, &command)).await;
            match result {
                Ok(Ok(result)) => {
                    if events.send(result.into_event()).await.is_err() {
                        return;
                    }
                    completed = true;
                    break;
                }
                Ok(Err(error)) if is_known_application_result(&error) => {
                    let event = failure_event(&command, error.to_string(), false);
                    if events.send(event).await.is_err() {
                        return;
                    }
                    completed = true;
                    break;
                }
                Ok(Err(error)) if is_reconnectable(&error) => {
                    last_error = Some(error.to_string());
                }
                Ok(Err(error)) => {
                    let event = failure_event(&command, error.to_string(), true);
                    if events.send(event).await.is_err() {
                        return;
                    }
                    completed = true;
                    break;
                }
                Err(_) => last_error = Some("local application request timed out".to_owned()),
            }

            if attempt + 1 < MAX_REQUEST_ATTEMPTS {
                time::sleep(RECONNECT_DELAY).await;
                match connect_or_start().await {
                    Ok(connected) => {
                        let server_version = connected.server_version().to_owned();
                        client = ApplicationClient::from_negotiated_connection(
                            connected.into_connection(),
                        );
                        if events
                            .send(RequestEvent::ConnectionRestored { server_version })
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(error) => last_error = Some(error.to_string()),
                }
            }
        }
        if !completed {
            let error = last_error.unwrap_or_else(|| "local application request failed".to_owned());
            if events
                .send(failure_event(&command, error, true))
                .await
                .is_err()
            {
                return;
            }
        }
    }
}

async fn send_credential_result(
    client: &mut Client,
    command: RequestCommand,
    events: &mpsc::Sender<RequestEvent>,
) -> Result<(), ()> {
    let Some(mutation_request_id) = command.mutation_request_id() else {
        return Err(());
    };
    let context = command.context();
    let result = time::timeout(REQUEST_TIMEOUT, execute_credential(client, command)).await;
    let (event, connection_unknown) = match result {
        Ok(Ok(result)) => (result.into_event(), false),
        Ok(Err(error)) => {
            let outcome_unknown = !is_known_application_result(&error);
            (
                RequestEvent::CredentialMutationFailed {
                    mutation_request_id,
                    context,
                    error: error.to_string(),
                    outcome_unknown,
                },
                outcome_unknown,
            )
        }
        Err(_) => (
            RequestEvent::CredentialMutationFailed {
                mutation_request_id,
                context,
                error: "local application request timed out".to_owned(),
                outcome_unknown: true,
            },
            true,
        ),
    };
    if connection_unknown {
        client.invalidate();
    }
    events.send(event).await.map_err(|_| ())
}

async fn execute_credential(
    client: &mut Client,
    command: RequestCommand,
) -> Result<RequestResult, ApplicationClientError> {
    match command {
        RequestCommand::SetCredential {
            mutation_request_id,
            expected_generation,
            api_key,
        } => client
            .set_open_code_credential(mutation_request_id, expected_generation, api_key)
            .await
            .map(|credential| RequestResult::CredentialUpdated {
                mutation_request_id,
                credential,
            }),
        RequestCommand::RemoveCredential {
            mutation_request_id,
            expected_generation,
        } => client
            .remove_open_code_credential(mutation_request_id, expected_generation)
            .await
            .map(|credential| RequestResult::CredentialUpdated {
                mutation_request_id,
                credential,
            }),
        RequestCommand::LoadSessions
        | RequestCommand::LoadModels(_)
        | RequestCommand::LoadDefaultModel
        | RequestCommand::LoadSettings
        | RequestCommand::LoadCredentialStatus
        | RequestCommand::LoadSession(_)
        | RequestCommand::LoadTranscriptWindow { .. }
        | RequestCommand::LoadContext { .. }
        | RequestCommand::SetDefaultModel { .. }
        | RequestCommand::SetSubagentModel { .. }
        | RequestCommand::CreateSession { .. }
        | RequestCommand::RenameSession { .. }
        | RequestCommand::SetSessionArchived { .. }
        | RequestCommand::DeleteSession { .. }
        | RequestCommand::SubmitInput { .. }
        | RequestCommand::ExecuteLocalCommand { .. }
        | RequestCommand::CancelRun { .. }
        | RequestCommand::CancelLocalCommand { .. }
        | RequestCommand::StopServer { .. } => {
            unreachable!("only credential mutations use credential execution")
        }
    }
}

async fn execute(
    client: &mut Client,
    command: &RequestCommand,
) -> Result<RequestResult, ApplicationClientError> {
    match command {
        RequestCommand::LoadSessions => load_sessions(client).await.map(RequestResult::Sessions),
        RequestCommand::LoadModels(service) => {
            client
                .list_open_code_models(*service)
                .await
                .map(|models| RequestResult::Models {
                    service: *service,
                    models,
                })
        }
        RequestCommand::LoadDefaultModel => client
            .default_open_code_model()
            .await
            .map(RequestResult::DefaultModel),
        RequestCommand::LoadSettings => client
            .application_settings()
            .await
            .map(RequestResult::Settings),
        RequestCommand::LoadCredentialStatus => client
            .open_code_credential_status()
            .await
            .map(RequestResult::CredentialStatus),
        RequestCommand::LoadSession(session_id) => load_session(client, *session_id)
            .await
            .map(RequestResult::Session),
        RequestCommand::LoadTranscriptWindow { session_id, target } => {
            load_transcript_window(client, *session_id, *target)
                .await
                .map(|window| RequestResult::TranscriptWindow {
                    target: *target,
                    window,
                })
        }
        RequestCommand::LoadContext {
            session_id,
            service,
            model_id,
        } => client
            .session_context_status(*session_id, *service, model_id.clone())
            .await
            .map(RequestResult::Context),
        RequestCommand::SetDefaultModel {
            mutation_request_id,
            service,
            model_id,
        } => client
            .set_default_open_code_model(*mutation_request_id, *service, model_id.clone())
            .await
            .map(|selection| RequestResult::DefaultModelUpdated {
                mutation_request_id: *mutation_request_id,
                selection,
            }),
        RequestCommand::SetSubagentModel {
            mutation_request_id,
            setting,
        } => client
            .set_subagent_model_setting(*mutation_request_id, setting.clone())
            .await
            .map(|settings| RequestResult::SettingsUpdated {
                mutation_request_id: *mutation_request_id,
                settings,
            }),
        RequestCommand::CreateSession {
            mutation_request_id,
        } => client
            .create_session(*mutation_request_id, None)
            .await
            .map(|session| RequestResult::SessionCreated {
                mutation_request_id: *mutation_request_id,
                session,
            }),
        RequestCommand::RenameSession {
            mutation_request_id,
            session_id,
            display_name,
        } => client
            .rename_session(*mutation_request_id, *session_id, display_name.clone())
            .await
            .map(|session| RequestResult::SessionRenamed {
                mutation_request_id: *mutation_request_id,
                session,
            }),
        RequestCommand::SetSessionArchived {
            mutation_request_id,
            session_id,
            archived,
        } => client
            .set_session_archived(*mutation_request_id, *session_id, *archived)
            .await
            .map(|session| RequestResult::SessionArchiveChanged {
                mutation_request_id: *mutation_request_id,
                session,
            }),
        RequestCommand::DeleteSession {
            mutation_request_id,
            session_id,
        } => client
            .delete_session(*mutation_request_id, *session_id)
            .await
            .map(|()| RequestResult::SessionDeleted {
                mutation_request_id: *mutation_request_id,
                session_id: *session_id,
            }),
        RequestCommand::SubmitInput {
            mutation_request_id,
            session_id,
            text,
            attachments,
            service,
            model_id,
        } => client
            .submit_session_input_with_images(
                *mutation_request_id,
                *session_id,
                text.clone(),
                attachments.clone(),
                *service,
                model_id.clone(),
            )
            .await
            .map(|accepted| RequestResult::InputAccepted {
                mutation_request_id: *mutation_request_id,
                accepted,
            }),
        RequestCommand::ExecuteLocalCommand {
            mutation_request_id,
            session_id,
            command,
            context_visible,
        } => client
            .execute_local_command(
                *mutation_request_id,
                *session_id,
                command.clone(),
                *context_visible,
            )
            .await
            .map(|accepted| RequestResult::LocalCommandAccepted {
                mutation_request_id: *mutation_request_id,
                session_id: *session_id,
                command_id: accepted.command_id,
            }),
        RequestCommand::CancelRun {
            mutation_request_id,
            session_id,
            run_id,
        } => client
            .cancel_run(*mutation_request_id, *session_id, *run_id)
            .await
            .map(|result| RequestResult::CancellationResolved {
                mutation_request_id: *mutation_request_id,
                result,
            }),
        RequestCommand::CancelLocalCommand {
            mutation_request_id,
            session_id,
            command_id,
        } => client
            .cancel_local_command(*mutation_request_id, *session_id, *command_id)
            .await
            .map(|result| RequestResult::LocalCommandCancellationResolved {
                mutation_request_id: *mutation_request_id,
                result,
            }),
        RequestCommand::SetCredential { .. } | RequestCommand::RemoveCredential { .. } => {
            unreachable!("credential mutations use single-attempt execution")
        }
        RequestCommand::StopServer {
            mutation_request_id,
        } => client
            .stop_server(*mutation_request_id)
            .await
            .map(|result| RequestResult::ServerStopAccepted {
                mutation_request_id: *mutation_request_id,
                result,
            }),
    }
}

async fn load_sessions(
    client: &mut Client,
) -> Result<(Vec<SessionSummary>, SessionCatalogEventCursor), ApplicationClientError> {
    let mut sessions = Vec::new();
    let mut cursor = None;
    let mut catalog_cursor = None;
    for _ in 0..MAX_SESSION_PAGES {
        let page = client.list_sessions(cursor, SESSION_PAGE_SIZE).await?;
        if catalog_cursor.is_some_and(|cursor| cursor != page.catalog_cursor) {
            return Err(ApplicationClientError::EventScopeMismatch);
        }
        catalog_cursor = Some(page.catalog_cursor);
        sessions.extend(page.sessions);
        cursor = page.next_cursor;
        if cursor.is_none() {
            return Ok((
                sessions,
                catalog_cursor.unwrap_or_else(SessionCatalogEventCursor::beginning),
            ));
        }
    }
    Err(ApplicationClientError::Application(
        ApplicationError::ResourceLimit {
            resource: morons_protocol::ResourceLimit::Sessions,
        },
    ))
}

async fn load_session(
    client: &mut Client,
    session_id: SessionId,
) -> Result<SessionSnapshot, ApplicationClientError> {
    let skill_catalog = client.list_session_skills(session_id).await?;
    let window = load_transcript_window(client, session_id, TranscriptWindowTarget::Latest).await?;
    Ok(SessionSnapshot {
        window,
        skills: skill_catalog.skills,
        skill_warnings: skill_catalog.warnings,
    })
}

async fn load_transcript_window(
    client: &mut Client,
    session_id: SessionId,
    target: TranscriptWindowTarget,
) -> Result<TranscriptWindow, ApplicationClientError> {
    let (mut cursor, direction) = target.request();
    let mut entries = Vec::new();
    let mut runs = Vec::new();
    let mut session = None;
    let mut event_cursor = None;
    let mut active_run_id = None;
    let mut active_command_id = None;
    let mut older_cursor = None;
    let mut newer_cursor = None;

    for index in 0..TRANSCRIPT_WINDOW_ENTRIES {
        let page = client
            .list_session_transcript(session_id, cursor, direction, TRANSCRIPT_ENTRY_PAGE_SIZE)
            .await?;
        if session
            .as_ref()
            .is_some_and(|session: &SessionSummary| session != &page.session)
            || event_cursor.is_some_and(|event_cursor| event_cursor != page.event_cursor)
            || index > 0
                && (active_run_id != page.active_run_id
                    || active_command_id != page.active_command_id)
        {
            return Err(ApplicationClientError::EventScopeMismatch);
        }
        if index == 0 {
            active_run_id = page.active_run_id;
            active_command_id = page.active_command_id;
            match direction {
                TranscriptPageDirection::Older => newer_cursor = page.newer_cursor,
                TranscriptPageDirection::Newer => older_cursor = page.older_cursor,
            }
        }
        session = Some(page.session);
        event_cursor = Some(page.event_cursor);
        entries.extend(page.entries);
        for run in page.runs {
            match runs
                .iter()
                .position(|existing: &RunSummary| existing.id == run.id)
            {
                Some(existing) if runs[existing] != run => {
                    return Err(ApplicationClientError::EventScopeMismatch);
                }
                Some(_) => {}
                None => runs.push(run),
            }
        }
        cursor = match direction {
            TranscriptPageDirection::Older => {
                older_cursor = page.older_cursor;
                page.older_cursor
            }
            TranscriptPageDirection::Newer => {
                newer_cursor = page.newer_cursor;
                page.newer_cursor
            }
        };
        if cursor.is_none() {
            break;
        }
    }
    if direction == TranscriptPageDirection::Older {
        entries.reverse();
    }
    Ok(TranscriptWindow {
        session: session.ok_or(ApplicationClientError::EventScopeMismatch)?,
        entries,
        runs,
        active_run_id,
        active_command_id,
        older_cursor,
        newer_cursor,
        event_cursor: event_cursor.ok_or(ApplicationClientError::EventScopeMismatch)?,
    })
}

enum RequestResult {
    Sessions((Vec<SessionSummary>, SessionCatalogEventCursor)),
    Models {
        service: OpenCodeService,
        models: Vec<OpenCodeModelSummary>,
    },
    DefaultModel(Option<OpenCodeModelSelection>),
    DefaultModelUpdated {
        mutation_request_id: MutationRequestId,
        selection: OpenCodeModelSelection,
    },
    Settings(ApplicationSettings),
    SettingsUpdated {
        mutation_request_id: MutationRequestId,
        settings: ApplicationSettings,
    },
    CredentialStatus(OpenCodeCredentialStatus),
    Session(SessionSnapshot),
    TranscriptWindow {
        target: TranscriptWindowTarget,
        window: TranscriptWindow,
    },
    Context(SessionContextStatus),
    SessionCreated {
        mutation_request_id: MutationRequestId,
        session: SessionSummary,
    },
    SessionRenamed {
        mutation_request_id: MutationRequestId,
        session: SessionSummary,
    },
    SessionArchiveChanged {
        mutation_request_id: MutationRequestId,
        session: SessionSummary,
    },
    SessionDeleted {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
    },
    InputAccepted {
        mutation_request_id: MutationRequestId,
        accepted: SessionInputAcceptance,
    },
    LocalCommandAccepted {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        command_id: LocalCommandId,
    },
    CancellationResolved {
        mutation_request_id: MutationRequestId,
        result: RunCancellationResult,
    },
    LocalCommandCancellationResolved {
        mutation_request_id: MutationRequestId,
        result: LocalCommandCancellationResult,
    },
    CredentialUpdated {
        mutation_request_id: MutationRequestId,
        credential: OpenCodeCredentialStatus,
    },
    ServerStopAccepted {
        mutation_request_id: MutationRequestId,
        result: ServerStopAcceptance,
    },
}

impl RequestResult {
    fn into_event(self) -> RequestEvent {
        match self {
            Self::Sessions((sessions, cursor)) => RequestEvent::SessionsLoaded { sessions, cursor },
            Self::Models { service, models } => RequestEvent::ModelsLoaded { service, models },
            Self::DefaultModel(selection) => RequestEvent::DefaultModelLoaded(selection),
            Self::DefaultModelUpdated {
                mutation_request_id,
                selection,
            } => RequestEvent::DefaultModelUpdated {
                mutation_request_id,
                selection,
            },
            Self::Settings(settings) => RequestEvent::SettingsLoaded(settings),
            Self::SettingsUpdated {
                mutation_request_id,
                settings,
            } => RequestEvent::SettingsUpdated {
                mutation_request_id,
                settings,
            },
            Self::CredentialStatus(status) => RequestEvent::CredentialStatusLoaded(status),
            Self::Session(snapshot) => RequestEvent::SessionLoaded(snapshot),
            Self::TranscriptWindow { target, window } => {
                RequestEvent::TranscriptWindowLoaded { target, window }
            }
            Self::Context(context) => RequestEvent::ContextLoaded(context),
            Self::SessionCreated {
                mutation_request_id,
                session,
            } => RequestEvent::SessionCreated {
                mutation_request_id,
                session,
            },
            Self::SessionRenamed {
                mutation_request_id,
                session,
            } => RequestEvent::SessionRenamed {
                mutation_request_id,
                session,
            },
            Self::SessionArchiveChanged {
                mutation_request_id,
                session,
            } => RequestEvent::SessionArchiveChanged {
                mutation_request_id,
                session,
            },
            Self::SessionDeleted {
                mutation_request_id,
                session_id,
            } => RequestEvent::SessionDeleted {
                mutation_request_id,
                session_id,
            },
            Self::InputAccepted {
                mutation_request_id,
                accepted,
            } => RequestEvent::InputAccepted {
                mutation_request_id,
                accepted,
            },
            Self::LocalCommandAccepted {
                mutation_request_id,
                session_id,
                command_id,
            } => RequestEvent::LocalCommandAccepted {
                mutation_request_id,
                session_id,
                command_id,
            },
            Self::CancellationResolved {
                mutation_request_id,
                result,
            } => RequestEvent::CancellationResolved {
                mutation_request_id,
                result,
            },
            Self::LocalCommandCancellationResolved {
                mutation_request_id,
                result,
            } => RequestEvent::LocalCommandCancellationResolved {
                mutation_request_id,
                command_id: result.command_id,
                cancellation_requested: result.cancellation_requested,
            },
            Self::CredentialUpdated {
                mutation_request_id,
                credential,
            } => RequestEvent::CredentialUpdated {
                mutation_request_id,
                credential,
            },
            Self::ServerStopAccepted {
                mutation_request_id,
                result,
            } => RequestEvent::ServerStopAccepted {
                mutation_request_id,
                result,
            },
        }
    }
}

fn failure_event(command: &RequestCommand, error: String, outcome_unknown: bool) -> RequestEvent {
    match command.mutation_request_id() {
        Some(mutation_request_id) if outcome_unknown => RequestEvent::MutationOutcomeUnknown {
            mutation_request_id,
            context: command.context(),
            error,
        },
        Some(mutation_request_id) => RequestEvent::MutationFailed {
            mutation_request_id,
            context: command.context(),
            error,
        },
        None => RequestEvent::QueryFailed {
            context: command.context(),
            model_service: match command {
                RequestCommand::LoadModels(service) => Some(*service),
                RequestCommand::LoadSessions
                | RequestCommand::LoadDefaultModel
                | RequestCommand::LoadSettings
                | RequestCommand::LoadCredentialStatus
                | RequestCommand::LoadSession(_)
                | RequestCommand::LoadTranscriptWindow { .. }
                | RequestCommand::LoadContext { .. } => None,
                RequestCommand::SetDefaultModel { .. }
                | RequestCommand::SetSubagentModel { .. }
                | RequestCommand::CreateSession { .. }
                | RequestCommand::RenameSession { .. }
                | RequestCommand::SetSessionArchived { .. }
                | RequestCommand::DeleteSession { .. }
                | RequestCommand::SubmitInput { .. }
                | RequestCommand::ExecuteLocalCommand { .. }
                | RequestCommand::CancelRun { .. }
                | RequestCommand::CancelLocalCommand { .. }
                | RequestCommand::SetCredential { .. }
                | RequestCommand::RemoveCredential { .. }
                | RequestCommand::StopServer { .. } => None,
            },
            error,
        },
    }
}

fn is_known_application_result(error: &ApplicationClientError) -> bool {
    matches!(error, ApplicationClientError::Application(_))
}

fn is_reconnectable(error: &ApplicationClientError) -> bool {
    matches!(
        error,
        ApplicationClientError::ServerDisconnected
            | ApplicationClientError::ConnectionUnusable
            | ApplicationClientError::RequestIdentifierExhausted
            | ApplicationClientError::Frame(FrameError::Io(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_commands_expose_only_their_stable_identifier() {
        let mutation_request_id = MutationRequestId::from_bytes([0x11; 16]);
        let command = RequestCommand::SubmitInput {
            mutation_request_id,
            session_id: SessionId::from_bytes([0x22; 16]),
            text: "sensitive prompt".to_owned(),
            attachments: Vec::new(),
            service: OpenCodeService::Zen,
            model_id: "grok-4.6".to_owned(),
        };
        assert_eq!(command.mutation_request_id(), Some(mutation_request_id));
        assert_eq!(command.context(), "message submission");
        assert!(command.clone_for_retry().is_some());

        let model = RequestCommand::SetDefaultModel {
            mutation_request_id,
            service: OpenCodeService::Go,
            model_id: "grok-4.6".to_owned(),
        };
        assert_eq!(model.mutation_request_id(), Some(mutation_request_id));
        assert_eq!(model.context(), "default model selection");
        assert!(matches!(
            model.clone_for_retry(),
            Some(RequestCommand::SetDefaultModel {
                mutation_request_id: retried,
                service: OpenCodeService::Go,
                ref model_id,
            }) if retried == mutation_request_id && model_id == "grok-4.6"
        ));

        let setting = RequestCommand::SetSubagentModel {
            mutation_request_id,
            setting: SubagentModelSetting::OpenCode {
                service: OpenCodeService::Go,
                model_id: "glm-5.3-flash".to_owned(),
            },
        };
        assert_eq!(setting.mutation_request_id(), Some(mutation_request_id));
        assert_eq!(setting.context(), "subagent model setting");
        assert!(matches!(
            setting.clone_for_retry(),
            Some(RequestCommand::SetSubagentModel {
                mutation_request_id: retried,
                setting: SubagentModelSetting::OpenCode {
                    service: OpenCodeService::Go,
                    ref model_id,
                },
            }) if retried == mutation_request_id && model_id == "glm-5.3-flash"
        ));
    }

    #[test]
    fn credential_mutations_cannot_enter_the_automatic_retry_path() {
        let mutation_request_id = MutationRequestId::from_bytes([0x33; 16]);
        let set = RequestCommand::SetCredential {
            mutation_request_id,
            expected_generation: 2,
            api_key: OpenCodeApiKey::new("not-a-real-key")
                .expect("test credential should be valid"),
        };
        assert!(set.is_credential_mutation());
        assert!(set.clone_for_retry().is_none());
        assert_eq!(set.mutation_request_id(), Some(mutation_request_id));

        let remove = RequestCommand::RemoveCredential {
            mutation_request_id,
            expected_generation: 2,
        };
        assert!(remove.is_credential_mutation());
        assert!(remove.clone_for_retry().is_none());
    }
}
