mod subscriptions;

pub use subscriptions::{SessionCatalogSubscription, SessionSubscription};

use std::{collections::BTreeSet, error::Error, fmt, path::Path};

const MAX_MODEL_SUMMARIES: usize = 256;
const MAX_MODEL_METADATA_BYTES: usize = 128;
const MAX_MODEL_TOKEN_LIMIT: u32 = 1_000_000;
const MAX_SESSION_DISPLAY_NAME_BYTES: usize = 256;

use morons_protocol::{
    ApplicationError, ApplicationRequest, ApplicationResponse, ClientMessage, FrameError,
    LocalCommandId, MessageId, MutationRequestId, OpenCodeApiKey, OpenCodeCredentialStatus,
    OpenCodeModelSummary, OpenCodeService, ResourceLimit, RunId, RunState, RunSummary,
    ServerMessage, SessionCatalogEventCursor, SessionEventCursor, SessionId, SessionListCursor,
    SessionSummary, TranscriptCursor, TranscriptEntry, WorkspaceBlockReason, WorkspaceState,
    WorkspaceSummary, read_server_message, write_client_message,
};
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPage {
    pub sessions: Vec<SessionSummary>,
    pub next_cursor: Option<SessionListCursor>,
    pub catalog_cursor: SessionCatalogEventCursor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInputAcceptance {
    pub user_message_id: MessageId,
    pub run: RunSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptPage {
    pub session: SessionSummary,
    pub workspace: WorkspaceSummary,
    pub entries: Vec<TranscriptEntry>,
    pub runs: Vec<RunSummary>,
    pub active_run_id: Option<RunId>,
    pub active_command_id: Option<LocalCommandId>,
    pub next_cursor: Option<TranscriptCursor>,
    pub event_cursor: SessionEventCursor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerStopAcceptance {
    pub current_server_stopping: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalCommandAcceptance {
    pub command_id: LocalCommandId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalCommandCancellationResult {
    pub command_id: LocalCommandId,
    pub cancellation_requested: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunCancellationResult {
    pub run_id: RunId,
    pub state: RunState,
    pub cancellation_requested: bool,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum ApplicationClientError {
    Frame(FrameError),
    ServerDisconnected,
    ConnectionUnusable,
    WorkingDirectoryUnavailable,
    RequestIdentifierExhausted,
    ResponseIdentifierMismatch {
        expected_request_id: u64,
        received_request_id: u64,
    },
    UnexpectedServerMessage,
    UnexpectedApplicationResponse,
    SubscriptionCursorMismatch,
    EventScopeMismatch,
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
            Self::WorkingDirectoryUnavailable => {
                formatter.write_str("the selected working directory is unavailable or unsupported")
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
                formatter.write_str("server accepted a different subscription scope or cursor")
            }
            Self::EventScopeMismatch => {
                formatter.write_str("server returned an event outside the subscription scope")
            }
            Self::EventCursorNotMonotonic => {
                formatter.write_str("server returned a non-monotonic subscription cursor")
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
            | Self::WorkingDirectoryUnavailable
            | Self::RequestIdentifierExhausted
            | Self::ResponseIdentifierMismatch { .. }
            | Self::UnexpectedServerMessage
            | Self::UnexpectedApplicationResponse
            | Self::SubscriptionCursorMismatch
            | Self::EventScopeMismatch
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
        ApplicationError::RunNotFound => formatter.write_str("run was not found"),
        ApplicationError::LocalCommandNotFound => {
            formatter.write_str("local command was not found")
        }
        ApplicationError::SessionBusy { active_run_id } => {
            write!(formatter, "session is busy with {active_run_id:?}")
        }
        ApplicationError::SessionCommandBusy { active_command_id } => {
            write!(formatter, "session is busy with {active_command_id:?}")
        }
        ApplicationError::WorkingDirectoryUnavailable => {
            formatter.write_str("the session working directory is unavailable")
        }
        ApplicationError::UnsupportedModel => {
            formatter.write_str("selected OpenCode model is unsupported")
        }
        ApplicationError::OpenCodeCredentialNotConfigured => {
            formatter.write_str("OpenCode credential is not configured")
        }
        ApplicationError::CredentialGenerationConflict => {
            formatter.write_str("OpenCode credential state changed")
        }
        ApplicationError::CredentialMutationNotApplied => {
            formatter.write_str("OpenCode credential update was not applied")
        }
        ApplicationError::WorkspaceBlocked => formatter.write_str("session workspace is blocked"),
        ApplicationError::ResourceLimit {
            resource: ResourceLimit::Sessions,
        } => formatter.write_str("session limit was reached"),
        ApplicationError::ResourceLimit {
            resource: ResourceLimit::Runs,
        } => formatter.write_str("agent run capacity was reached"),
        ApplicationError::ResourceLimit {
            resource: ResourceLimit::Context,
        } => formatter.write_str("agent context limit was reached"),
        ApplicationError::ResourceLimit {
            resource: ResourceLimit::Storage,
        } => formatter.write_str("session storage limit was reached"),
        ApplicationError::ServiceUnavailable => {
            formatter.write_str("application service is unavailable")
        }
        ApplicationError::Internal => formatter.write_str("application request failed internally"),
    }
}

fn valid_session_summary(session: &SessionSummary) -> bool {
    session.id.as_bytes().iter().any(|byte| *byte != 0)
        && session.display_name.as_ref().is_none_or(|display_name| {
            !display_name.is_empty() && display_name.len() <= MAX_SESSION_DISPLAY_NAME_BYTES
        })
        && session
            .working_directory
            .as_deref()
            .is_none_or(valid_working_directory)
}

fn valid_working_directory(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= morons_protocol::MAX_WORKING_DIRECTORY_PATH_BYTES
        && !path.chars().any(char::is_control)
        && Path::new(path).is_absolute()
        && !Path::new(path).components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
}

fn valid_workspace_summary(workspace: WorkspaceSummary) -> bool {
    match workspace.state {
        WorkspaceState::Empty | WorkspaceState::Importing => {
            workspace.file_count == 0
                && workspace.logical_bytes == 0
                && workspace.block_reason.is_none()
                && workspace.blocked_run_id.is_none()
                && workspace.blocked_tool.is_none()
        }
        WorkspaceState::Ready => {
            workspace.block_reason.is_none()
                && workspace.blocked_run_id.is_none()
                && workspace.blocked_tool.is_none()
        }
        WorkspaceState::Blocked => match workspace.block_reason {
            Some(WorkspaceBlockReason::InconsistentImportState) => {
                workspace.file_count == 0
                    && workspace.logical_bytes == 0
                    && workspace.blocked_run_id.is_none()
                    && workspace.blocked_tool.is_none()
            }
            Some(WorkspaceBlockReason::UncertainToolEffect) => {
                workspace.blocked_run_id.is_some() && workspace.blocked_tool.is_some()
            }
            None => false,
        },
    }
}

fn valid_model_summaries(service: OpenCodeService, models: &[OpenCodeModelSummary]) -> bool {
    if models.len() > MAX_MODEL_SUMMARIES {
        return false;
    }
    let mut identifiers = BTreeSet::new();
    models.iter().all(|model| {
        model.service == service
            && !model.id.is_empty()
            && model.id.len() <= MAX_MODEL_METADATA_BYTES
            && model.id.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'-' | b'_')
            })
            && identifiers.insert(model.id.as_str())
            && !model.display_name.is_empty()
            && model.display_name.len() <= MAX_MODEL_METADATA_BYTES
            && model.responses_protocol_revision > 0
            && model.maximum_input_tokens > 0
            && model.maximum_input_tokens <= MAX_MODEL_TOKEN_LIMIT
            && model.maximum_output_tokens > 0
            && model.maximum_output_tokens <= MAX_MODEL_TOKEN_LIMIT
            && model.capabilities.text_input
            && model.capabilities.text_output
    })
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
        let working_directory = std::env::current_dir()
            .map_err(|_| ApplicationClientError::WorkingDirectoryUnavailable)?
            .into_os_string()
            .into_string()
            .map_err(|_| ApplicationClientError::WorkingDirectoryUnavailable)?;
        self.create_session_at(mutation_request_id, display_name, working_directory)
            .await
    }

    pub async fn create_session_at(
        &mut self,
        mutation_request_id: MutationRequestId,
        display_name: Option<String>,
        working_directory: String,
    ) -> Result<SessionSummary, ApplicationClientError> {
        if !valid_working_directory(&working_directory) {
            return Err(ApplicationClientError::WorkingDirectoryUnavailable);
        }
        let response = self
            .request(ApplicationRequest::CreateSession {
                mutation_request_id,
                display_name: display_name.clone(),
                working_directory: working_directory.clone(),
            })
            .await?;
        let ApplicationResponse::SessionCreated { session } = response else {
            return Err(self.unexpected_application_response());
        };
        if !valid_session_summary(&session)
            || session.display_name != display_name
            || session.working_directory.as_deref() != Some(&working_directory)
        {
            self.usable = false;
            return Err(ApplicationClientError::EventScopeMismatch);
        }
        Ok(session)
    }

    pub async fn submit_session_input(
        &mut self,
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        text: String,
        service: OpenCodeService,
        model_id: String,
    ) -> Result<SessionInputAcceptance, ApplicationClientError> {
        let response = self
            .request(ApplicationRequest::SubmitSessionInput {
                mutation_request_id,
                session_id,
                text,
                service,
                model_id: model_id.clone(),
            })
            .await?;
        let ApplicationResponse::SessionInputAccepted {
            user_message_id,
            run,
        } = response
        else {
            return Err(self.unexpected_application_response());
        };
        if run.session_id != session_id
            || run.user_message_id != user_message_id
            || run.service != service
            || run.model_id != model_id
        {
            self.usable = false;
            return Err(ApplicationClientError::EventScopeMismatch);
        }
        Ok(SessionInputAcceptance {
            user_message_id,
            run,
        })
    }

    pub async fn execute_local_command(
        &mut self,
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        command: String,
        context_visible: bool,
    ) -> Result<LocalCommandAcceptance, ApplicationClientError> {
        let response = self
            .request(ApplicationRequest::ExecuteLocalCommand {
                mutation_request_id,
                session_id,
                command,
                context_visible,
            })
            .await?;
        let ApplicationResponse::LocalCommandAccepted { command_id } = response else {
            return Err(self.unexpected_application_response());
        };
        Ok(LocalCommandAcceptance { command_id })
    }

    pub async fn get_run(
        &mut self,
        session_id: SessionId,
        run_id: RunId,
    ) -> Result<Option<RunSummary>, ApplicationClientError> {
        let response = match self
            .request(ApplicationRequest::GetRun { session_id, run_id })
            .await
        {
            Ok(response) => response,
            Err(ApplicationClientError::Application(ApplicationError::RunNotFound)) => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let ApplicationResponse::RunFound { run } = response else {
            return Err(self.unexpected_application_response());
        };
        if run.id != run_id || run.session_id != session_id {
            self.usable = false;
            return Err(ApplicationClientError::EventScopeMismatch);
        }
        Ok(Some(run))
    }

    pub async fn list_session_transcript(
        &mut self,
        session_id: SessionId,
        cursor: Option<TranscriptCursor>,
        limit: u16,
    ) -> Result<TranscriptPage, ApplicationClientError> {
        let response = self
            .request(ApplicationRequest::ListSessionTranscript {
                session_id,
                cursor,
                limit,
            })
            .await?;
        let ApplicationResponse::SessionTranscriptListed {
            session,
            workspace,
            entries,
            runs,
            active_run_id,
            active_command_id,
            next_cursor,
            event_cursor,
        } = response
        else {
            return Err(self.unexpected_application_response());
        };
        let cursor_in_scope = event_cursor.as_bytes()[..16] == session_id.as_bytes()[..]
            && next_cursor
                .as_ref()
                .is_none_or(|cursor| cursor.as_bytes()[..16] == session_id.as_bytes()[..]);
        let snapshot_is_consistent = cursor.is_none_or(|cursor| {
            cursor.as_bytes()[24..32] == event_cursor.as_bytes()[16..]
                && next_cursor.as_ref().is_none_or(|next_cursor| {
                    next_cursor.as_bytes()[16..32] == cursor.as_bytes()[16..32]
                })
        }) && next_cursor.as_ref().is_none_or(|next_cursor| {
            next_cursor.as_bytes()[24..32] == event_cursor.as_bytes()[16..]
        });
        let runs_in_scope = runs.iter().all(|run| run.session_id == session_id);
        let runs_are_unique = runs
            .iter()
            .enumerate()
            .all(|(index, run)| runs[..index].iter().all(|prior| prior.id != run.id));
        let entries_have_runs = entries.iter().all(|entry| match entry {
            TranscriptEntry::UserMessage { id, run_id, .. } => runs
                .iter()
                .any(|run| run.id == *run_id && run.user_message_id == *id),
            TranscriptEntry::AssistantMessage {
                run_id,
                service,
                model_id,
                ..
            } => runs.iter().any(|run| {
                run.id == *run_id && run.service == *service && run.model_id == *model_id
            }),
            TranscriptEntry::ToolCall { run_id, .. }
            | TranscriptEntry::ToolResult { run_id, .. } => {
                runs.iter().any(|run| run.id == *run_id)
            }
            TranscriptEntry::LocalCommand { .. } => true,
        });
        let active_run_is_valid = active_run_id.is_none_or(|active_run_id| {
            runs.iter()
                .any(|run| run.id == active_run_id && !run.state.is_terminal())
        });
        if session.id != session_id
            || !valid_session_summary(&session)
            || !valid_workspace_summary(workspace)
            || !cursor_in_scope
            || !snapshot_is_consistent
            || entries.len() > usize::from(limit)
            || runs.len() > usize::from(limit).saturating_add(1)
            || !runs_in_scope
            || !runs_are_unique
            || !entries_have_runs
            || !active_run_is_valid
        {
            self.usable = false;
            return Err(ApplicationClientError::EventScopeMismatch);
        }
        Ok(TranscriptPage {
            session,
            workspace,
            entries,
            runs,
            active_run_id,
            active_command_id,
            next_cursor,
            event_cursor,
        })
    }

    pub async fn stop_server(
        &mut self,
        mutation_request_id: MutationRequestId,
    ) -> Result<ServerStopAcceptance, ApplicationClientError> {
        let response = self
            .request(ApplicationRequest::StopServer {
                mutation_request_id,
            })
            .await?;
        let ApplicationResponse::ServerStopAccepted {
            current_server_stopping,
        } = response
        else {
            return Err(self.unexpected_application_response());
        };
        self.usable = false;
        Ok(ServerStopAcceptance {
            current_server_stopping,
        })
    }

    pub async fn cancel_run(
        &mut self,
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        run_id: RunId,
    ) -> Result<RunCancellationResult, ApplicationClientError> {
        let response = self
            .request(ApplicationRequest::CancelRun {
                mutation_request_id,
                session_id,
                run_id,
            })
            .await?;
        let ApplicationResponse::RunCancellationResolved {
            run_id: resolved_run_id,
            state,
            cancellation_requested,
        } = response
        else {
            return Err(self.unexpected_application_response());
        };
        if resolved_run_id != run_id {
            self.usable = false;
            return Err(ApplicationClientError::EventScopeMismatch);
        }
        Ok(RunCancellationResult {
            run_id: resolved_run_id,
            state,
            cancellation_requested,
        })
    }

    pub async fn cancel_local_command(
        &mut self,
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        command_id: LocalCommandId,
    ) -> Result<LocalCommandCancellationResult, ApplicationClientError> {
        let response = self
            .request(ApplicationRequest::CancelLocalCommand {
                mutation_request_id,
                session_id,
                command_id,
            })
            .await?;
        let ApplicationResponse::LocalCommandCancellationResolved {
            command_id: resolved,
            cancellation_requested,
        } = response
        else {
            return Err(self.unexpected_application_response());
        };
        if resolved != command_id {
            self.usable = false;
            return Err(ApplicationClientError::EventScopeMismatch);
        }
        Ok(LocalCommandCancellationResult {
            command_id,
            cancellation_requested,
        })
    }

    pub async fn acknowledge_tool_uncertainty(
        &mut self,
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        run_id: RunId,
    ) -> Result<WorkspaceSummary, ApplicationClientError> {
        let response = self
            .request(ApplicationRequest::AcknowledgeToolUncertainty {
                mutation_request_id,
                session_id,
                run_id,
            })
            .await?;
        let ApplicationResponse::ToolUncertaintyAcknowledged {
            session_id: response_session_id,
            run_id: response_run_id,
            workspace,
        } = response
        else {
            return Err(self.unexpected_application_response());
        };
        if response_session_id != session_id
            || response_run_id != run_id
            || !valid_workspace_summary(workspace)
            || workspace.state != WorkspaceState::Ready
        {
            self.usable = false;
            return Err(ApplicationClientError::EventScopeMismatch);
        }
        Ok(workspace)
    }

    pub async fn list_open_code_models(
        &mut self,
        service: OpenCodeService,
    ) -> Result<Vec<OpenCodeModelSummary>, ApplicationClientError> {
        let response = self
            .request(ApplicationRequest::ListOpenCodeModels { service })
            .await?;
        let ApplicationResponse::OpenCodeModelsListed {
            service: response_service,
            models,
        } = response
        else {
            return Err(self.unexpected_application_response());
        };
        if response_service != service || !valid_model_summaries(service, &models) {
            self.usable = false;
            return Err(ApplicationClientError::EventScopeMismatch);
        }
        Ok(models)
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
        if session.id != session_id || !valid_session_summary(&session) {
            self.usable = false;
            return Err(ApplicationClientError::EventScopeMismatch);
        }
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
        let snapshot_is_consistent = cursor.is_none_or(|cursor| {
            cursor.as_bytes()[..8] == catalog_cursor.as_bytes()[..]
                && next_cursor
                    .as_ref()
                    .is_none_or(|next_cursor| next_cursor.as_bytes()[..8] == cursor.as_bytes()[..8])
        }) && next_cursor
            .as_ref()
            .is_none_or(|next_cursor| next_cursor.as_bytes()[..8] == catalog_cursor.as_bytes()[..]);
        let sessions_are_valid = sessions.iter().enumerate().all(|(index, session)| {
            valid_session_summary(session)
                && sessions[..index].iter().all(|prior| prior.id != session.id)
        });
        if sessions.len() > usize::from(limit) || !snapshot_is_consistent || !sessions_are_valid {
            self.usable = false;
            return Err(ApplicationClientError::EventScopeMismatch);
        }
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

    pub async fn subscribe_to_session(
        mut self,
        session_id: SessionId,
        cursor: SessionEventCursor,
    ) -> Result<SessionSubscription<S>, ApplicationClientError> {
        let response = self
            .request(ApplicationRequest::SubscribeSession { session_id, cursor })
            .await?;
        let ApplicationResponse::SessionSubscriptionStarted {
            session_id: accepted_session_id,
            cursor: accepted_cursor,
        } = response
        else {
            return Err(self.unexpected_application_response());
        };
        if accepted_session_id != session_id || accepted_cursor != cursor {
            self.usable = false;
            return Err(ApplicationClientError::SubscriptionCursorMismatch);
        }
        Ok(SessionSubscription {
            connection: self.connection,
            session_id,
            cursor,
            active_delta_run: None,
            terminal_delta_run: None,
            delta_sequence: 0,
            usable: self.usable,
        })
    }

    #[must_use]
    pub fn into_inner(self) -> S {
        self.connection
    }

    pub(crate) fn invalidate(&mut self) {
        self.usable = false;
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

#[cfg(test)]
mod tests;
