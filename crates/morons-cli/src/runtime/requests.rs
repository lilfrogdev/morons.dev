use std::time::Duration;

use interprocess::local_socket::tokio::Stream;
use morons_protocol::{
    ApplicationError, FrameError, MutationRequestId, OpenCodeApiKey, OpenCodeCredentialStatus,
    OpenCodeModelSummary, OpenCodeService, RunId, RunSummary, SessionCatalogEventCursor,
    SessionEventCursor, SessionId, SessionSummary, TranscriptEntry, WorkspaceSummary,
};
use tokio::{sync::mpsc, time};

use crate::{
    ApplicationClient, ApplicationClientError, RunCancellationResult, ServerStopAcceptance,
    SessionInputAcceptance, connect_or_start,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(35);
const RECONNECT_DELAY: Duration = Duration::from_millis(150);
const MAX_REQUEST_ATTEMPTS: usize = 3;
const SESSION_PAGE_SIZE: u16 = 100;
const MAX_SESSION_PAGES: usize = 100;
const TRANSCRIPT_PAGE_SIZE: u16 = 1;
const MAX_TRANSCRIPT_PAGES: usize = 512;

type Client = ApplicationClient<Stream>;

pub(super) enum RequestCommand {
    LoadSessions,
    LoadModels(OpenCodeService),
    LoadCredentialStatus,
    LoadSession(SessionId),
    CreateSession {
        mutation_request_id: MutationRequestId,
    },
    SubmitInput {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        text: String,
        service: OpenCodeService,
        model_id: String,
    },
    CancelRun {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        run_id: RunId,
    },
    ImportRepository {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        source_path: String,
    },
    AcknowledgeToolUncertainty {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        run_id: RunId,
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
            | Self::LoadCredentialStatus
            | Self::LoadSession(_) => None,
            Self::CreateSession {
                mutation_request_id,
            }
            | Self::SubmitInput {
                mutation_request_id,
                ..
            }
            | Self::CancelRun {
                mutation_request_id,
                ..
            }
            | Self::ImportRepository {
                mutation_request_id,
                ..
            }
            | Self::AcknowledgeToolUncertainty {
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
            Self::LoadCredentialStatus => "credential status",
            Self::LoadSession(_) => "session transcript",
            Self::CreateSession { .. } => "session creation",
            Self::SubmitInput { .. } => "message submission",
            Self::CancelRun { .. } => "run cancellation",
            Self::ImportRepository { .. } => "repository import",
            Self::AcknowledgeToolUncertainty { .. } => "tool uncertainty acknowledgement",
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
            Self::LoadCredentialStatus => Some(Self::LoadCredentialStatus),
            Self::LoadSession(session_id) => Some(Self::LoadSession(*session_id)),
            Self::CreateSession {
                mutation_request_id,
            } => Some(Self::CreateSession {
                mutation_request_id: *mutation_request_id,
            }),
            Self::SubmitInput {
                mutation_request_id,
                session_id,
                text,
                service,
                model_id,
            } => Some(Self::SubmitInput {
                mutation_request_id: *mutation_request_id,
                session_id: *session_id,
                text: text.clone(),
                service: *service,
                model_id: model_id.clone(),
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
            Self::ImportRepository {
                mutation_request_id,
                session_id,
                source_path,
            } => Some(Self::ImportRepository {
                mutation_request_id: *mutation_request_id,
                session_id: *session_id,
                source_path: source_path.clone(),
            }),
            Self::AcknowledgeToolUncertainty {
                mutation_request_id,
                session_id,
                run_id,
            } => Some(Self::AcknowledgeToolUncertainty {
                mutation_request_id: *mutation_request_id,
                session_id: *session_id,
                run_id: *run_id,
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
    CredentialStatusLoaded(OpenCodeCredentialStatus),
    SessionLoaded(SessionSnapshot),
    SessionCreated {
        mutation_request_id: MutationRequestId,
        session: SessionSummary,
    },
    InputAccepted {
        mutation_request_id: MutationRequestId,
        accepted: SessionInputAcceptance,
    },
    CancellationResolved {
        mutation_request_id: MutationRequestId,
        result: RunCancellationResult,
    },
    RepositoryImported {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        workspace: WorkspaceSummary,
    },
    ToolUncertaintyAcknowledged {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        workspace: WorkspaceSummary,
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

pub(super) struct SessionSnapshot {
    pub(super) session: SessionSummary,
    pub(super) workspace: WorkspaceSummary,
    pub(super) entries: Vec<TranscriptEntry>,
    pub(super) runs: Vec<RunSummary>,
    pub(super) active_run_id: Option<RunId>,
    pub(super) event_cursor: SessionEventCursor,
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
        | RequestCommand::LoadCredentialStatus
        | RequestCommand::LoadSession(_)
        | RequestCommand::CreateSession { .. }
        | RequestCommand::SubmitInput { .. }
        | RequestCommand::CancelRun { .. }
        | RequestCommand::ImportRepository { .. }
        | RequestCommand::AcknowledgeToolUncertainty { .. }
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
        RequestCommand::LoadCredentialStatus => client
            .open_code_credential_status()
            .await
            .map(RequestResult::CredentialStatus),
        RequestCommand::LoadSession(session_id) => load_session(client, *session_id)
            .await
            .map(RequestResult::Session),
        RequestCommand::CreateSession {
            mutation_request_id,
        } => client
            .create_session(*mutation_request_id, None)
            .await
            .map(|session| RequestResult::SessionCreated {
                mutation_request_id: *mutation_request_id,
                session,
            }),
        RequestCommand::SubmitInput {
            mutation_request_id,
            session_id,
            text,
            service,
            model_id,
        } => client
            .submit_session_input(
                *mutation_request_id,
                *session_id,
                text.clone(),
                *service,
                model_id.clone(),
            )
            .await
            .map(|accepted| RequestResult::InputAccepted {
                mutation_request_id: *mutation_request_id,
                accepted,
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
        RequestCommand::ImportRepository {
            mutation_request_id,
            session_id,
            source_path,
        } => client
            .import_repository(*mutation_request_id, *session_id, source_path.clone())
            .await
            .map(|workspace| RequestResult::RepositoryImported {
                mutation_request_id: *mutation_request_id,
                session_id: *session_id,
                workspace,
            }),
        RequestCommand::AcknowledgeToolUncertainty {
            mutation_request_id,
            session_id,
            run_id,
        } => client
            .acknowledge_tool_uncertainty(*mutation_request_id, *session_id, *run_id)
            .await
            .map(|workspace| RequestResult::ToolUncertaintyAcknowledged {
                mutation_request_id: *mutation_request_id,
                session_id: *session_id,
                workspace,
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
    let mut entries = Vec::new();
    let mut runs = Vec::new();
    let mut cursor = None;
    let mut session = None;
    let mut workspace = None;
    let mut event_cursor = None;
    let mut active_run_id = None;
    let mut snapshot_metadata_loaded = false;
    for _ in 0..MAX_TRANSCRIPT_PAGES {
        let page = client
            .list_session_transcript(session_id, cursor, TRANSCRIPT_PAGE_SIZE)
            .await?;
        if session
            .as_ref()
            .is_some_and(|session: &SessionSummary| session != &page.session)
            || workspace.is_some_and(|workspace| workspace != page.workspace)
            || event_cursor.is_some_and(|event_cursor| event_cursor != page.event_cursor)
            || snapshot_metadata_loaded && active_run_id != page.active_run_id
        {
            return Err(ApplicationClientError::EventScopeMismatch);
        }
        session = Some(page.session);
        workspace = Some(page.workspace);
        event_cursor = Some(page.event_cursor);
        if !snapshot_metadata_loaded {
            active_run_id = page.active_run_id;
            snapshot_metadata_loaded = true;
        }
        entries.extend(page.entries);
        for run in page.runs {
            match runs
                .iter()
                .position(|existing: &RunSummary| existing.id == run.id)
            {
                Some(index) if runs[index] != run => {
                    return Err(ApplicationClientError::EventScopeMismatch);
                }
                Some(_) => {}
                None => runs.push(run),
            }
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            return Ok(SessionSnapshot {
                session: session.ok_or(ApplicationClientError::EventScopeMismatch)?,
                workspace: workspace.ok_or(ApplicationClientError::EventScopeMismatch)?,
                entries,
                runs,
                active_run_id,
                event_cursor: event_cursor.ok_or(ApplicationClientError::EventScopeMismatch)?,
            });
        }
    }
    Err(ApplicationClientError::Application(
        ApplicationError::ResourceLimit {
            resource: morons_protocol::ResourceLimit::Storage,
        },
    ))
}

enum RequestResult {
    Sessions((Vec<SessionSummary>, SessionCatalogEventCursor)),
    Models {
        service: OpenCodeService,
        models: Vec<OpenCodeModelSummary>,
    },
    CredentialStatus(OpenCodeCredentialStatus),
    Session(SessionSnapshot),
    SessionCreated {
        mutation_request_id: MutationRequestId,
        session: SessionSummary,
    },
    InputAccepted {
        mutation_request_id: MutationRequestId,
        accepted: SessionInputAcceptance,
    },
    CancellationResolved {
        mutation_request_id: MutationRequestId,
        result: RunCancellationResult,
    },
    RepositoryImported {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        workspace: WorkspaceSummary,
    },
    ToolUncertaintyAcknowledged {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        workspace: WorkspaceSummary,
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
            Self::CredentialStatus(status) => RequestEvent::CredentialStatusLoaded(status),
            Self::Session(snapshot) => RequestEvent::SessionLoaded(snapshot),
            Self::SessionCreated {
                mutation_request_id,
                session,
            } => RequestEvent::SessionCreated {
                mutation_request_id,
                session,
            },
            Self::InputAccepted {
                mutation_request_id,
                accepted,
            } => RequestEvent::InputAccepted {
                mutation_request_id,
                accepted,
            },
            Self::CancellationResolved {
                mutation_request_id,
                result,
            } => RequestEvent::CancellationResolved {
                mutation_request_id,
                result,
            },
            Self::RepositoryImported {
                mutation_request_id,
                session_id,
                workspace,
            } => RequestEvent::RepositoryImported {
                mutation_request_id,
                session_id,
                workspace,
            },
            Self::ToolUncertaintyAcknowledged {
                mutation_request_id,
                session_id,
                workspace,
            } => RequestEvent::ToolUncertaintyAcknowledged {
                mutation_request_id,
                session_id,
                workspace,
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
                | RequestCommand::LoadCredentialStatus
                | RequestCommand::LoadSession(_) => None,
                RequestCommand::CreateSession { .. }
                | RequestCommand::SubmitInput { .. }
                | RequestCommand::CancelRun { .. }
                | RequestCommand::ImportRepository { .. }
                | RequestCommand::AcknowledgeToolUncertainty { .. }
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
            service: OpenCodeService::Zen,
            model_id: "grok-4.6".to_owned(),
        };
        assert_eq!(command.mutation_request_id(), Some(mutation_request_id));
        assert_eq!(command.context(), "message submission");
        assert!(command.clone_for_retry().is_some());
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
