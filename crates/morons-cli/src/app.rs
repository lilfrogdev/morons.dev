mod input;
mod render;

use std::{error::Error, fmt};

use morons_protocol::{
    ApplicationEvent, MessageId, OpenCodeApiKey, OpenCodeCredentialStatus, OpenCodeModelSummary,
    OpenCodeService, RunId, RunState, RunSummary, SessionId, SessionSummary, TranscriptEntry,
    WorkspaceSummary,
};
use ratatui::Frame;

use crate::terminal::{CredentialBuffer, PromptBuffer, RepositoryPathBuffer, SafeText};

const MAX_CLIENT_SESSIONS: usize = 10_000;
const MAX_CLIENT_TRANSCRIPT_ENTRIES: usize = 512;
const MAX_CLIENT_RUNS: usize = 512;
const MAX_TRANSIENT_DELTA_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum View {
    Sessions,
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingOperation {
    CreateSession,
    SubmitInput,
    CancelRun,
    ImportRepository,
    AcknowledgeToolUncertainty,
    UpdateCredential,
    StopServer,
}

pub(super) enum RepositoryDialog {
    Enter { input: RepositoryPathBuffer },
    Confirm { source_path: String },
}

pub(super) enum CredentialDialog {
    ChooseAction,
    Enter {
        replacing: bool,
        input: CredentialBuffer,
    },
    ConfirmRemove,
}

#[derive(PartialEq, Eq)]
pub(super) enum AppAction {
    None,
    Quit,
    Refresh,
    CreateSession,
    OpenSession(SessionId),
    CloseSession,
    SubmitInput {
        session_id: SessionId,
        text: String,
        service: OpenCodeService,
        model_id: String,
    },
    CancelRun {
        session_id: SessionId,
        run_id: RunId,
    },
    ImportRepository {
        session_id: SessionId,
        source_path: String,
    },
    AcknowledgeToolUncertainty {
        session_id: SessionId,
        run_id: RunId,
    },
    SetCredential {
        expected_generation: u64,
        api_key: OpenCodeApiKey,
    },
    RemoveCredential {
        expected_generation: u64,
    },
    StopServer,
    RetryPending,
    AbandonPending,
}

impl fmt::Debug for AppAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("None"),
            Self::Quit => formatter.write_str("Quit"),
            Self::Refresh => formatter.write_str("Refresh"),
            Self::CreateSession => formatter.write_str("CreateSession"),
            Self::OpenSession(session_id) => formatter
                .debug_tuple("OpenSession")
                .field(session_id)
                .finish(),
            Self::CloseSession => formatter.write_str("CloseSession"),
            Self::SubmitInput {
                session_id,
                text,
                service,
                model_id,
            } => formatter
                .debug_struct("SubmitInput")
                .field("session_id", session_id)
                .field("text_bytes", &text.len())
                .field("service", service)
                .field("model_id", model_id)
                .finish(),
            Self::CancelRun { session_id, run_id } => formatter
                .debug_struct("CancelRun")
                .field("session_id", session_id)
                .field("run_id", run_id)
                .finish(),
            Self::ImportRepository {
                session_id,
                source_path,
            } => formatter
                .debug_struct("ImportRepository")
                .field("session_id", session_id)
                .field("source_path_bytes", &source_path.len())
                .finish(),
            Self::AcknowledgeToolUncertainty { session_id, run_id } => formatter
                .debug_struct("AcknowledgeToolUncertainty")
                .field("session_id", session_id)
                .field("run_id", run_id)
                .finish(),
            Self::SetCredential {
                expected_generation,
                ..
            } => formatter
                .debug_struct("SetCredential")
                .field("expected_generation", expected_generation)
                .field("api_key", &"[REDACTED]")
                .finish(),
            Self::RemoveCredential {
                expected_generation,
            } => formatter
                .debug_struct("RemoveCredential")
                .field("expected_generation", expected_generation)
                .finish(),
            Self::StopServer => formatter.write_str("StopServer"),
            Self::RetryPending => formatter.write_str("RetryPending"),
            Self::AbandonPending => formatter.write_str("AbandonPending"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UiStateError {
    ResourceScopeMismatch,
    ResourceLimitExceeded,
    InvalidRunTransition,
    InvalidWorkspaceTransition,
}

impl fmt::Display for UiStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ResourceScopeMismatch => "received application state outside the selected scope",
            Self::ResourceLimitExceeded => "application state exceeded a client resource limit",
            Self::InvalidRunTransition => "received an invalid run transition",
            Self::InvalidWorkspaceTransition => "received an invalid workspace transition",
        })
    }
}

impl Error for UiStateError {}

pub(super) struct AppState {
    pub(super) server_version: SafeText,
    pub(super) status: SafeText,
    pub(super) sessions: Vec<PresentedSession>,
    pub(super) selected_session: usize,
    pub(super) models: Vec<PresentedModel>,
    pub(super) selected_model: Option<usize>,
    pub(super) credential: Option<OpenCodeCredentialStatus>,
    pub(super) credential_dialog: Option<CredentialDialog>,
    pub(super) repository_dialog: Option<RepositoryDialog>,
    pub(super) view: View,
    pub(super) session: Option<SessionView>,
    pub(super) prompt: PromptBuffer,
    pub(super) pending: Option<PendingOperation>,
    pub(super) pending_unknown: bool,
    pub(super) confirm_stop: bool,
    pub(super) confirm_uncertainty: bool,
    pub(super) transcript_scroll: u16,
}

impl AppState {
    pub(super) fn new(server_version: &str) -> Self {
        Self {
            server_version: SafeText::from_untrusted(server_version),
            status: SafeText::from_untrusted("Loading local state"),
            sessions: Vec::new(),
            selected_session: 0,
            models: Vec::new(),
            selected_model: None,
            credential: None,
            credential_dialog: None,
            repository_dialog: None,
            view: View::Sessions,
            session: None,
            prompt: PromptBuffer::default(),
            pending: None,
            pending_unknown: false,
            confirm_stop: false,
            confirm_uncertainty: false,
            transcript_scroll: 0,
        }
    }

    pub(super) fn render(&self, frame: &mut Frame<'_>) {
        render::render(frame, self);
    }

    pub(super) fn set_status(&mut self, status: impl AsRef<str>) {
        self.status = SafeText::from_untrusted(status.as_ref());
    }

    pub(super) fn set_credential_status(&mut self, credential: OpenCodeCredentialStatus) {
        self.credential = Some(credential);
    }

    pub(super) fn clear_credential_interaction(&mut self) {
        self.credential_dialog = None;
    }

    pub(super) fn clear_repository_interaction(&mut self) {
        self.repository_dialog = None;
    }

    pub(super) fn mark_credential_status_unknown(&mut self) {
        self.credential = None;
        self.clear_credential_interaction();
    }

    pub(super) fn replace_sessions(
        &mut self,
        sessions: Vec<SessionSummary>,
    ) -> Result<(), UiStateError> {
        if sessions.len() > MAX_CLIENT_SESSIONS {
            return Err(UiStateError::ResourceLimitExceeded);
        }
        let selected_id = self
            .sessions
            .get(self.selected_session)
            .map(|session| session.summary.id);
        self.sessions = sessions.into_iter().map(PresentedSession::new).collect();
        self.selected_session = selected_id
            .and_then(|selected_id| {
                self.sessions
                    .iter()
                    .position(|session| session.summary.id == selected_id)
            })
            .unwrap_or(0)
            .min(self.sessions.len().saturating_sub(1));
        Ok(())
    }

    pub(super) fn add_session(&mut self, session: SessionSummary) -> Result<(), UiStateError> {
        if self
            .sessions
            .iter()
            .any(|existing| existing.summary.id == session.id)
        {
            return Ok(());
        }
        if self.sessions.len() >= MAX_CLIENT_SESSIONS {
            return Err(UiStateError::ResourceLimitExceeded);
        }
        self.sessions.push(PresentedSession::new(session));
        self.selected_session = self.sessions.len() - 1;
        Ok(())
    }

    pub(super) fn replace_models(
        &mut self,
        service: OpenCodeService,
        models: Vec<OpenCodeModelSummary>,
    ) -> Result<(), UiStateError> {
        if models.iter().any(|model| model.service != service) {
            return Err(UiStateError::ResourceScopeMismatch);
        }
        let selected = self
            .selected_model()
            .map(|model| (model.model.service, model.model.id.as_str().to_owned()));
        self.models.retain(|model| model.model.service != service);
        self.models
            .extend(models.into_iter().map(PresentedModel::new));
        self.selected_model = selected
            .and_then(|(service, id)| {
                self.models.iter().position(|model| {
                    model.model.available && model.model.service == service && model.model.id == id
                })
            })
            .or_else(|| self.models.iter().position(|model| model.model.available));
        Ok(())
    }

    pub(super) fn open_session(
        &mut self,
        summary: SessionSummary,
        workspace: WorkspaceSummary,
        entries: Vec<TranscriptEntry>,
        runs: Vec<RunSummary>,
        active_run_id: Option<RunId>,
    ) -> Result<(), UiStateError> {
        let session = SessionView::new(summary, workspace, entries, runs, active_run_id)?;
        self.session = Some(session);
        self.view = View::Session;
        self.prompt.clear();
        self.clear_repository_interaction();
        self.transcript_scroll = 0;
        Ok(())
    }

    pub(super) fn close_session(&mut self) {
        self.view = View::Sessions;
        self.session = None;
        self.prompt.clear();
        self.clear_repository_interaction();
        self.pending = None;
        self.pending_unknown = false;
        self.transcript_scroll = 0;
    }

    pub(super) fn apply_event(&mut self, event: ApplicationEvent) -> Result<(), UiStateError> {
        match event {
            ApplicationEvent::SessionCreated { session, .. } => self.add_session(session),
            ApplicationEvent::SessionTranscriptEntryCommitted {
                session_id, entry, ..
            } => self.session_mut(session_id)?.append_transcript_entry(entry),
            ApplicationEvent::SessionRunChanged { run, .. } => {
                self.session_mut(run.session_id)?.apply_run(run)
            }
            ApplicationEvent::SessionWorkspaceChanged {
                session_id,
                workspace,
                ..
            } => self.session_mut(session_id)?.apply_workspace(workspace),
            ApplicationEvent::SessionAssistantDelta {
                session_id,
                run_id,
                delta,
                refusal,
                ..
            } => self
                .session_mut(session_id)?
                .append_delta(run_id, &delta, refusal),
        }
    }

    pub(super) fn mark_pending(&mut self, operation: PendingOperation) {
        self.pending = Some(operation);
        self.pending_unknown = false;
    }

    pub(super) fn mark_pending_unknown(&mut self) {
        if self.pending.is_some() {
            self.pending_unknown = true;
        }
    }

    pub(super) fn clear_pending(&mut self) {
        self.pending = None;
        self.pending_unknown = false;
    }

    pub(super) fn session_input_accepted(&mut self, run: RunSummary) -> Result<(), UiStateError> {
        self.prompt.clear();
        self.clear_pending();
        self.session_mut(run.session_id)?.apply_run(run)
    }

    pub(super) fn repository_imported(
        &mut self,
        session_id: SessionId,
        workspace: WorkspaceSummary,
    ) -> Result<(), UiStateError> {
        self.clear_pending();
        self.session_mut(session_id)?.apply_workspace(workspace)
    }

    pub(super) fn cancellation_resolved(
        &mut self,
        run_id: RunId,
        state: RunState,
        cancellation_requested: bool,
    ) -> Result<(), UiStateError> {
        self.clear_pending();
        let session = self
            .session
            .as_mut()
            .ok_or(UiStateError::ResourceScopeMismatch)?;
        let run = session
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .ok_or(UiStateError::ResourceScopeMismatch)?;
        run.state = state;
        run.cancellation_requested = cancellation_requested;
        if state.is_terminal() && session.active_run_id == Some(run_id) {
            session.active_run_id = None;
            session.transient = None;
        }
        Ok(())
    }

    pub(super) fn clear_transient_assistant(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.transient = None;
        }
    }

    pub(super) fn selected_session_id(&self) -> Option<SessionId> {
        self.sessions
            .get(self.selected_session)
            .map(|session| session.summary.id)
    }

    pub(super) fn selected_model(&self) -> Option<&PresentedModel> {
        self.selected_model.and_then(|index| self.models.get(index))
    }

    fn session_mut(&mut self, session_id: SessionId) -> Result<&mut SessionView, UiStateError> {
        self.session
            .as_mut()
            .filter(|session| session.summary.id == session_id)
            .ok_or(UiStateError::ResourceScopeMismatch)
    }
}

pub(super) struct PresentedSession {
    pub(super) summary: SessionSummary,
    pub(super) display_name: SafeText,
}

impl PresentedSession {
    fn new(summary: SessionSummary) -> Self {
        let display_name = SafeText::from_untrusted(
            summary
                .display_name
                .as_deref()
                .unwrap_or("Untitled session"),
        );
        Self {
            summary,
            display_name,
        }
    }
}

pub(super) struct PresentedModel {
    pub(super) id: SafeText,
    pub(super) display_name: SafeText,
    pub(super) model: OpenCodeModelSummary,
}

impl PresentedModel {
    fn new(model: OpenCodeModelSummary) -> Self {
        Self {
            id: SafeText::from_untrusted(&model.id),
            display_name: SafeText::from_untrusted(&model.display_name),
            model,
        }
    }
}

pub(super) struct SessionView {
    pub(super) summary: SessionSummary,
    pub(super) workspace: WorkspaceSummary,
    pub(super) display_name: SafeText,
    pub(super) entries: Vec<PresentedTranscriptEntry>,
    pub(super) runs: Vec<RunSummary>,
    pub(super) active_run_id: Option<RunId>,
    pub(super) transient: Option<TransientAssistant>,
}

impl SessionView {
    fn new(
        summary: SessionSummary,
        workspace: WorkspaceSummary,
        entries: Vec<TranscriptEntry>,
        runs: Vec<RunSummary>,
        active_run_id: Option<RunId>,
    ) -> Result<Self, UiStateError> {
        if entries.len() > MAX_CLIENT_TRANSCRIPT_ENTRIES || runs.len() > MAX_CLIENT_RUNS {
            return Err(UiStateError::ResourceLimitExceeded);
        }
        if runs.iter().any(|run| run.session_id != summary.id)
            || entries.iter().any(|entry| {
                let run_id = transcript_entry_run_id(entry);
                !runs.iter().any(|run| run.id == run_id)
            })
            || active_run_id.is_some_and(|active_run_id| {
                !runs
                    .iter()
                    .any(|run| run.id == active_run_id && !run.state.is_terminal())
            })
        {
            return Err(UiStateError::ResourceScopeMismatch);
        }
        let display_name = SafeText::from_untrusted(
            summary
                .display_name
                .as_deref()
                .unwrap_or("Untitled session"),
        );
        Ok(Self {
            summary,
            workspace,
            display_name,
            entries: entries
                .into_iter()
                .map(PresentedTranscriptEntry::new)
                .collect(),
            runs,
            active_run_id,
            transient: None,
        })
    }

    fn apply_workspace(&mut self, workspace: WorkspaceSummary) -> Result<(), UiStateError> {
        if self.workspace.state == morons_protocol::WorkspaceState::Ready
            && workspace.state == morons_protocol::WorkspaceState::Importing
        {
            return Ok(());
        }
        let valid = matches!(
            (self.workspace.state, workspace.state),
            (
                morons_protocol::WorkspaceState::Empty,
                morons_protocol::WorkspaceState::Importing
            ) | (
                morons_protocol::WorkspaceState::Empty,
                morons_protocol::WorkspaceState::Ready
            ) | (
                morons_protocol::WorkspaceState::Importing,
                morons_protocol::WorkspaceState::Empty
            ) | (
                morons_protocol::WorkspaceState::Importing,
                morons_protocol::WorkspaceState::Ready
            ) | (
                morons_protocol::WorkspaceState::Importing,
                morons_protocol::WorkspaceState::Blocked
            ) | (
                morons_protocol::WorkspaceState::Ready,
                morons_protocol::WorkspaceState::Ready
            ) | (
                morons_protocol::WorkspaceState::Ready,
                morons_protocol::WorkspaceState::Blocked
            ) | (
                morons_protocol::WorkspaceState::Blocked,
                morons_protocol::WorkspaceState::Ready
            ) | (
                morons_protocol::WorkspaceState::Blocked,
                morons_protocol::WorkspaceState::Blocked
            )
        );
        if !valid {
            return Err(UiStateError::InvalidWorkspaceTransition);
        }
        self.workspace = workspace;
        Ok(())
    }

    fn append_transcript_entry(&mut self, entry: TranscriptEntry) -> Result<(), UiStateError> {
        let id = transcript_entry_id(&entry);
        if self.entries.iter().any(|existing| existing.id == id) {
            return Ok(());
        }
        if self.entries.len() >= MAX_CLIENT_TRANSCRIPT_ENTRIES {
            return Err(UiStateError::ResourceLimitExceeded);
        }
        let run_id = transcript_entry_run_id(&entry);
        if matches!(entry, TranscriptEntry::AssistantMessage { .. })
            && self
                .transient
                .as_ref()
                .is_some_and(|transient| transient.run_id == run_id)
        {
            self.transient = None;
        }
        self.entries.push(PresentedTranscriptEntry::new(entry));
        Ok(())
    }

    fn apply_run(&mut self, run: RunSummary) -> Result<(), UiStateError> {
        if run.session_id != self.summary.id {
            return Err(UiStateError::ResourceScopeMismatch);
        }
        if let Some(existing) = self.runs.iter_mut().find(|existing| existing.id == run.id) {
            if !valid_run_transition(existing.state, run.state) {
                return Err(UiStateError::InvalidRunTransition);
            }
            *existing = run.clone();
        } else {
            if self.runs.len() >= MAX_CLIENT_RUNS {
                return Err(UiStateError::ResourceLimitExceeded);
            }
            self.runs.push(run.clone());
        }
        if run.state.is_terminal() {
            if self.active_run_id == Some(run.id) {
                self.active_run_id = None;
            }
            if self
                .transient
                .as_ref()
                .is_some_and(|transient| transient.run_id == run.id)
            {
                self.transient = None;
            }
        } else {
            if self
                .active_run_id
                .is_some_and(|active_run| active_run != run.id)
            {
                return Err(UiStateError::InvalidRunTransition);
            }
            self.active_run_id = Some(run.id);
        }
        Ok(())
    }

    fn append_delta(
        &mut self,
        run_id: RunId,
        delta: &str,
        refusal: bool,
    ) -> Result<(), UiStateError> {
        if self.active_run_id != Some(run_id) {
            return Err(UiStateError::InvalidRunTransition);
        }
        let transient = self
            .transient
            .get_or_insert_with(|| TransientAssistant::new(run_id, refusal));
        if transient.run_id != run_id || transient.refusal != refusal {
            return Err(UiStateError::InvalidRunTransition);
        }
        let Some(next_length) = transient.text.len().checked_add(delta.len()) else {
            transient.truncated = true;
            return Ok(());
        };
        if next_length > MAX_TRANSIENT_DELTA_BYTES {
            transient.truncated = true;
            return Ok(());
        }
        transient.text.push_str(delta);
        transient.presented = SafeText::from_untrusted(&transient.text);
        Ok(())
    }
}

pub(super) struct PresentedTranscriptEntry {
    pub(super) id: MessageId,
    pub(super) role: &'static str,
    pub(super) text: SafeText,
    pub(super) refusal: bool,
}

impl PresentedTranscriptEntry {
    fn new(entry: TranscriptEntry) -> Self {
        match entry {
            TranscriptEntry::UserMessage { id, text, .. } => Self {
                id,
                role: "You",
                text: SafeText::from_untrusted(&text),
                refusal: false,
            },
            TranscriptEntry::AssistantMessage {
                id, text, refusal, ..
            } => Self {
                id,
                role: "Assistant",
                text: SafeText::from_untrusted(&text),
                refusal,
            },
            TranscriptEntry::ToolCall { id, tool, path, .. } => Self {
                id,
                role: "Tool call",
                text: SafeText::from_untrusted(&format!("{} · {path}", tool_label(tool))),
                refusal: false,
            },
            TranscriptEntry::ToolResult {
                id,
                tool,
                status,
                summary,
                ..
            } => Self {
                id,
                role: "Tool result",
                text: SafeText::from_untrusted(&format!(
                    "{} · {status:?} · {summary}",
                    tool_label(tool)
                )),
                refusal: false,
            },
        }
    }
}

pub(super) struct TransientAssistant {
    pub(super) run_id: RunId,
    text: String,
    pub(super) presented: SafeText,
    pub(super) refusal: bool,
    pub(super) truncated: bool,
}

impl TransientAssistant {
    fn new(run_id: RunId, refusal: bool) -> Self {
        Self {
            run_id,
            text: String::new(),
            presented: SafeText::default(),
            refusal,
            truncated: false,
        }
    }
}

impl fmt::Debug for TransientAssistant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransientAssistant")
            .field("run_id", &self.run_id)
            .field("text_bytes", &self.text.len())
            .field("refusal", &self.refusal)
            .field("truncated", &self.truncated)
            .finish()
    }
}

const fn valid_run_transition(previous: RunState, next: RunState) -> bool {
    match previous {
        RunState::Accepted => true,
        RunState::Active => !matches!(next, RunState::Accepted),
        RunState::Succeeded => matches!(next, RunState::Succeeded),
        RunState::Failed => matches!(next, RunState::Failed),
        RunState::Cancelled => matches!(next, RunState::Cancelled),
        RunState::Interrupted => matches!(next, RunState::Interrupted),
        RunState::Uncertain => matches!(next, RunState::Uncertain),
    }
}

fn transcript_entry_id(entry: &TranscriptEntry) -> MessageId {
    match entry {
        TranscriptEntry::UserMessage { id, .. }
        | TranscriptEntry::AssistantMessage { id, .. }
        | TranscriptEntry::ToolCall { id, .. }
        | TranscriptEntry::ToolResult { id, .. } => *id,
    }
}

fn transcript_entry_run_id(entry: &TranscriptEntry) -> RunId {
    match entry {
        TranscriptEntry::UserMessage { run_id, .. }
        | TranscriptEntry::AssistantMessage { run_id, .. }
        | TranscriptEntry::ToolCall { run_id, .. }
        | TranscriptEntry::ToolResult { run_id, .. } => *run_id,
    }
}

const fn tool_label(tool: morons_protocol::ToolKind) -> &'static str {
    match tool {
        morons_protocol::ToolKind::ListDirectory => "list_directory",
        morons_protocol::ToolKind::ReadFile => "read_file",
        morons_protocol::ToolKind::SearchText => "search_text",
        morons_protocol::ToolKind::EditFile => "edit_file",
        morons_protocol::ToolKind::CreateFile => "create_file",
        morons_protocol::ToolKind::CreateDirectory => "create_directory",
    }
}

#[cfg(test)]
mod tests;
