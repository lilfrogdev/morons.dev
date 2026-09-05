mod context;
mod input;
mod render;
mod viewport;

use std::{error::Error, fmt};

use morons_protocol::{
    ApplicationEvent, ApplicationSettings, LocalCommandId, MessageId, OpenCodeApiKey,
    OpenCodeCredentialStatus, OpenCodeModelSelection, OpenCodeModelSummary, OpenCodeService,
    RunFailureKind, RunId, RunState, RunSummary, SessionContextStatus, SessionId, SessionSummary,
    SkillSummary, SubagentModelSetting, TranscriptCursor, TranscriptEntry,
};
use ratatui::Frame;

use self::viewport::{TranscriptBlockKey, TranscriptViewport};
use crate::terminal::{CredentialBuffer, PromptBuffer, SafeText, is_bidirectional_control};

const MAX_CLIENT_SESSIONS: usize = 10_000;
const TRANSCRIPT_WINDOW_TARGET_ENTRIES: usize = 64;
const MAX_CLIENT_TRANSCRIPT_ENTRIES: usize = TRANSCRIPT_WINDOW_TARGET_ENTRIES * 2;
const MAX_CLIENT_RUNS: usize = 512;
const MAX_TRANSIENT_DELTA_BYTES: usize = 128 * 1024;
const MAX_DRAFT_IMAGES: usize = 4;
const MAX_DRAFT_IMAGE_BYTES: usize = 6 * 1024 * 1024;
const MAX_IMAGE_DISPLAY_NAME_BYTES: usize = 128;
const MAX_MODEL_SEARCH_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum View {
    Sessions,
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingOperation {
    CreateSession,
    SelectModel,
    UpdateSettings,
    SubmitInput,
    RenameSession,
    ArchiveSession,
    DeleteSession,
    ExecuteLocalCommand,
    CancelRun,
    CancelLocalCommand,
    UpdateCredential,
    StopServer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InformationDialog {
    Context,
    TrustNotice,
    Help,
}

pub(super) struct ModelDialog {
    pub(super) query: PromptBuffer,
    pub(super) selected: usize,
}

pub(super) enum SettingsDialog {
    Overview,
    SubagentModel {
        query: PromptBuffer,
        selected: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SubagentModelCandidate {
    InheritParent,
    Model(usize),
}

pub(super) enum CredentialDialog {
    ChooseAction,
    Enter {
        replacing: bool,
        input: CredentialBuffer,
    },
    ConfirmRemove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TranscriptNavigation {
    Latest,
    Oldest,
    Older(TranscriptCursor),
    Newer(TranscriptCursor),
}

pub(super) struct TranscriptWindowData {
    pub(super) summary: SessionSummary,
    pub(super) entries: Vec<TranscriptEntry>,
    pub(super) runs: Vec<RunSummary>,
    pub(super) active_run_id: Option<RunId>,
    pub(super) active_command_id: Option<LocalCommandId>,
    pub(super) older_cursor: Option<TranscriptCursor>,
    pub(super) newer_cursor: Option<TranscriptCursor>,
}

#[derive(PartialEq, Eq)]
pub(super) enum AppAction {
    None,
    Quit,
    Refresh,
    CreateSession,
    OpenSession(SessionId),
    CloseSession,
    NavigateTranscript {
        session_id: SessionId,
        navigation: TranscriptNavigation,
    },
    RenameSession {
        session_id: SessionId,
        display_name: String,
    },
    SetSessionArchived {
        session_id: SessionId,
        archived: bool,
    },
    DeleteSession {
        session_id: SessionId,
    },
    ShowContext {
        session_id: SessionId,
        service: OpenCodeService,
        model_id: String,
    },
    LoadSettings,
    SetDefaultModel {
        service: OpenCodeService,
        model_id: String,
    },
    SetSubagentModel {
        setting: SubagentModelSetting,
    },
    SubmitInput {
        session_id: SessionId,
        text: String,
        attachments: Vec<morons_protocol::ImageUpload>,
        service: OpenCodeService,
        model_id: String,
    },
    ExecuteLocalCommand {
        session_id: SessionId,
        command: String,
        context_visible: bool,
    },
    CancelRun {
        session_id: SessionId,
        run_id: RunId,
    },
    CancelLocalCommand {
        session_id: SessionId,
        command_id: LocalCommandId,
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
            Self::NavigateTranscript {
                session_id,
                navigation,
            } => formatter
                .debug_struct("NavigateTranscript")
                .field("session_id", session_id)
                .field("navigation", navigation)
                .finish(),
            Self::RenameSession {
                session_id,
                display_name,
            } => formatter
                .debug_struct("RenameSession")
                .field("session_id", session_id)
                .field("display_name_bytes", &display_name.len())
                .finish(),
            Self::SetSessionArchived {
                session_id,
                archived,
            } => formatter
                .debug_struct("SetSessionArchived")
                .field("session_id", session_id)
                .field("archived", archived)
                .finish(),
            Self::DeleteSession { session_id } => formatter
                .debug_tuple("DeleteSession")
                .field(session_id)
                .finish(),
            Self::ShowContext {
                session_id,
                service,
                model_id,
            } => formatter
                .debug_struct("ShowContext")
                .field("session_id", session_id)
                .field("service", service)
                .field("model_id", model_id)
                .finish(),
            Self::LoadSettings => formatter.write_str("LoadSettings"),
            Self::SetDefaultModel { service, model_id } => formatter
                .debug_struct("SetDefaultModel")
                .field("service", service)
                .field("model_id", model_id)
                .finish(),
            Self::SetSubagentModel { setting } => formatter
                .debug_struct("SetSubagentModel")
                .field("setting", setting)
                .finish(),
            Self::SubmitInput {
                session_id,
                text,
                attachments,
                service,
                model_id,
            } => formatter
                .debug_struct("SubmitInput")
                .field("session_id", session_id)
                .field("text_bytes", &text.len())
                .field("attachments", &attachments.len())
                .field("service", service)
                .field("model_id", model_id)
                .finish(),
            Self::ExecuteLocalCommand {
                session_id,
                command,
                context_visible,
            } => formatter
                .debug_struct("ExecuteLocalCommand")
                .field("session_id", session_id)
                .field("command_bytes", &command.len())
                .field("context_visible", context_visible)
                .finish(),
            Self::CancelRun { session_id, run_id } => formatter
                .debug_struct("CancelRun")
                .field("session_id", session_id)
                .field("run_id", run_id)
                .finish(),
            Self::CancelLocalCommand {
                session_id,
                command_id,
            } => formatter
                .debug_struct("CancelLocalCommand")
                .field("session_id", session_id)
                .field("command_id", command_id)
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
}

impl fmt::Display for UiStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ResourceScopeMismatch => "received application state outside the selected scope",
            Self::ResourceLimitExceeded => "application state exceeded a client resource limit",
            Self::InvalidRunTransition => "received an invalid run transition",
        })
    }
}

impl Error for UiStateError {}

#[derive(Clone, Copy)]
pub(super) struct TerminalRunPresentation {
    pub(super) heading: &'static str,
    pub(super) detail: &'static str,
}

pub(super) const fn terminal_run_presentation(run: &RunSummary) -> Option<TerminalRunPresentation> {
    let (heading, detail) = match run.state {
        RunState::Failed => (
            "Run failed",
            match run.failure {
                Some(RunFailureKind::CredentialChanged) => {
                    "Credential changed before provider dispatch"
                }
                Some(RunFailureKind::CredentialNotConfigured) => {
                    "OpenCode credential is not configured"
                }
                Some(RunFailureKind::AuthenticationOrEntitlement) => {
                    "Authentication or account entitlement was rejected"
                }
                Some(RunFailureKind::RateLimited) => "Provider rate limit reached",
                Some(RunFailureKind::ProviderUnavailable) => "Provider is unavailable",
                Some(RunFailureKind::ProviderRejected) => "Provider rejected the request",
                Some(RunFailureKind::ProviderProtocol) => {
                    "Provider response violated the expected protocol"
                }
                Some(RunFailureKind::InvalidProviderOutput) => "Model output was invalid",
                Some(RunFailureKind::ToolExecution) => "Tool execution failed",
                Some(RunFailureKind::ResourceLimit) => "Run exceeded a resource limit",
                Some(RunFailureKind::Internal) => "Morons encountered an internal failure",
                None => "Failure reason is unavailable",
            },
        ),
        RunState::Cancelled => ("Run cancelled", "Cancellation completed"),
        RunState::Interrupted => ("Run interrupted", "Run stopped before completion"),
        RunState::Uncertain => (
            "Run outcome uncertain",
            "An external effect may have occurred; inspect the working directory",
        ),
        RunState::Accepted | RunState::Active | RunState::Succeeded => return None,
    };
    Some(TerminalRunPresentation { heading, detail })
}

pub(super) const fn service_label(service: OpenCodeService) -> &'static str {
    match service {
        OpenCodeService::Zen => "Zen",
        OpenCodeService::Go => "Go",
    }
}

const fn model_catalog_index(service: OpenCodeService) -> usize {
    match service {
        OpenCodeService::Zen => 0,
        OpenCodeService::Go => 1,
    }
}

fn model_search_score(model: &PresentedModel, query: &str) -> Option<u32> {
    if query.trim().is_empty() {
        return Some(0);
    }
    let service = service_label(model.model.service).to_lowercase();
    let id = model.model.id.to_lowercase();
    let display_name = model.model.display_name.to_lowercase();
    let canonical = format!("{service}/{id}");
    let search = format!("{service} {id} {canonical} {display_name}");
    query
        .to_lowercase()
        .split_whitespace()
        .try_fold(0_u32, |score, term| {
            let term_score = if term == service || term == id || term == canonical {
                0
            } else if id.starts_with(term) || canonical.starts_with(term) {
                1
            } else if id.contains(term) || canonical.contains(term) {
                2
            } else if display_name.contains(term) {
                3
            } else if search.contains(term) {
                4
            } else if is_character_subsequence(&search, term) {
                16
            } else {
                return None;
            };
            score.checked_add(term_score)
        })
}

fn is_character_subsequence(value: &str, query: &str) -> bool {
    let mut characters = value.chars();
    query
        .chars()
        .all(|expected| characters.by_ref().any(|character| character == expected))
}

struct DraftImage {
    upload: morons_protocol::ImageUpload,
    normalized_bytes: usize,
}

pub(super) struct AppState {
    pub(super) server_version: SafeText,
    pub(super) status: SafeText,
    pub(super) sessions: Vec<PresentedSession>,
    pub(super) selected_session: usize,
    pub(super) models: Vec<PresentedModel>,
    pub(super) selected_model: Option<usize>,
    pub(super) default_model: Option<OpenCodeModelSelection>,
    pub(super) loaded_model_catalogs: [bool; 2],
    pub(super) model_dialog: Option<ModelDialog>,
    pub(super) settings: Option<ApplicationSettings>,
    pub(super) settings_dialog: Option<SettingsDialog>,
    pub(super) credential: Option<OpenCodeCredentialStatus>,
    pub(super) credential_dialog: Option<CredentialDialog>,
    pub(super) information_dialog: Option<InformationDialog>,
    pub(super) rename_dialog: Option<PromptBuffer>,
    pub(super) view: View,
    pub(super) session: Option<SessionView>,
    pub(super) prompt: PromptBuffer,
    draft_images: Vec<DraftImage>,
    pub(super) pending: Option<PendingOperation>,
    pub(super) pending_unknown: bool,
    pub(super) confirm_stop: bool,
    pub(super) confirm_delete: Option<SessionId>,
    transcript_viewport: TranscriptViewport,
    transcript_page_loading: bool,
    pub(super) skill_completion_index: usize,
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
            default_model: None,
            loaded_model_catalogs: [false; 2],
            model_dialog: None,
            settings: None,
            settings_dialog: None,
            credential: None,
            credential_dialog: None,
            information_dialog: initial_information_dialog(),
            rename_dialog: None,
            view: View::Sessions,
            session: None,
            prompt: PromptBuffer::default(),
            draft_images: Vec::new(),
            pending: None,
            pending_unknown: false,
            confirm_stop: false,
            confirm_delete: None,
            transcript_viewport: TranscriptViewport::default(),
            transcript_page_loading: false,
            skill_completion_index: 0,
        }
    }

    pub(super) fn render(&mut self, frame: &mut Frame<'_>) {
        render::render(frame, self);
    }

    pub(super) fn scroll_transcript_lines_up(&mut self, rows: usize) -> AppAction {
        if self.transcript_viewport.at_top() {
            return self.navigate_to_older_window();
        }
        self.transcript_viewport.scroll_lines_up(rows);
        AppAction::None
    }

    pub(super) fn scroll_transcript_lines_down(&mut self, rows: usize) -> AppAction {
        if self.transcript_viewport.at_bottom() {
            return self.navigate_to_newer_window();
        }
        self.transcript_viewport.scroll_lines_down(rows);
        AppAction::None
    }

    pub(super) fn scroll_transcript_page_up(&mut self) -> AppAction {
        if self.transcript_viewport.at_top() {
            return self.navigate_to_older_window();
        }
        self.transcript_viewport.scroll_page_up();
        AppAction::None
    }

    pub(super) fn scroll_transcript_page_down(&mut self) -> AppAction {
        if self.transcript_viewport.at_bottom() {
            return self.navigate_to_newer_window();
        }
        self.transcript_viewport.scroll_page_down();
        AppAction::None
    }

    pub(super) fn scroll_transcript_to_start(&mut self) -> AppAction {
        let Some(session) = self.session.as_ref() else {
            return AppAction::None;
        };
        if session.older_cursor.is_some() && !self.transcript_page_loading {
            self.transcript_page_loading = true;
            return AppAction::NavigateTranscript {
                session_id: session.summary.id,
                navigation: TranscriptNavigation::Oldest,
            };
        }
        self.transcript_viewport.scroll_to_top();
        AppAction::None
    }

    pub(super) fn scroll_transcript_to_latest(&mut self) -> AppAction {
        let Some(session) = self.session.as_ref() else {
            return AppAction::None;
        };
        if (session.newer_cursor.is_some() || session.deferred_newer_output)
            && !self.transcript_page_loading
        {
            self.transcript_page_loading = true;
            return AppAction::NavigateTranscript {
                session_id: session.summary.id,
                navigation: TranscriptNavigation::Latest,
            };
        }
        self.transcript_viewport.scroll_to_bottom();
        AppAction::None
    }

    fn navigate_to_older_window(&mut self) -> AppAction {
        let Some(session) = self.session.as_ref() else {
            return AppAction::None;
        };
        let Some(cursor) = session.older_cursor else {
            return AppAction::None;
        };
        if self.transcript_page_loading {
            return AppAction::None;
        }
        self.transcript_page_loading = true;
        AppAction::NavigateTranscript {
            session_id: session.summary.id,
            navigation: TranscriptNavigation::Older(cursor),
        }
    }

    fn navigate_to_newer_window(&mut self) -> AppAction {
        let Some(session) = self.session.as_ref() else {
            return AppAction::None;
        };
        if self.transcript_page_loading {
            return AppAction::None;
        }
        let navigation = if let Some(cursor) = session.newer_cursor {
            TranscriptNavigation::Newer(cursor)
        } else if session.deferred_newer_output {
            TranscriptNavigation::Latest
        } else {
            return AppAction::None;
        };
        self.transcript_page_loading = true;
        AppAction::NavigateTranscript {
            session_id: session.summary.id,
            navigation,
        }
    }

    pub(super) fn skill_completion(&self) -> Option<(Vec<&PresentedSkill>, usize)> {
        let prefix = self.prompt.skill_completion_prefix()?;
        let matches = self
            .session
            .as_ref()?
            .skills
            .iter()
            .filter(|skill| skill.name.starts_with(prefix))
            .collect::<Vec<_>>();
        if matches.is_empty() {
            return None;
        }
        let selected = self.skill_completion_index.min(matches.len() - 1);
        Some((matches, selected))
    }

    pub(super) fn cycle_skill_completion(&mut self, reverse: bool) -> bool {
        let Some((matches, selected)) = self.skill_completion() else {
            return false;
        };
        self.skill_completion_index = if reverse {
            selected.checked_sub(1).unwrap_or(matches.len() - 1)
        } else {
            (selected + 1) % matches.len()
        };
        true
    }

    pub(super) fn complete_selected_skill(&mut self) -> bool {
        let Some((matches, selected)) = self.skill_completion() else {
            return false;
        };
        let name = matches[selected].name.clone();
        let completed = self.prompt.complete_skill(&name);
        if completed {
            self.skill_completion_index = 0;
        }
        completed
    }

    pub(super) fn reset_skill_completion(&mut self) {
        self.skill_completion_index = 0;
    }

    pub(super) fn accepts_image_input(&self) -> bool {
        self.view == View::Session
            && self.pending.is_none()
            && self.credential_dialog.is_none()
            && self.model_dialog.is_none()
            && self.settings_dialog.is_none()
            && !self.confirm_stop
    }

    pub(super) fn add_draft_image(
        &mut self,
        image: morons_image::NormalizedImage,
        suggested_name: Option<&str>,
    ) {
        if !self.accepts_image_input() {
            return;
        }
        if self.draft_images.len() >= MAX_DRAFT_IMAGES
            || self
                .draft_images
                .iter()
                .map(|image| image.normalized_bytes)
                .sum::<usize>()
                .checked_add(image.bytes.len())
                .is_none_or(|bytes| bytes > MAX_DRAFT_IMAGE_BYTES)
        {
            self.set_status("Image attachment limit reached");
            return;
        }
        let base_name = suggested_name
            .map(sanitize_image_name)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| {
                format!(
                    "pasted-image-{}.{}",
                    self.draft_images.len() + 1,
                    image.media_type.extension()
                )
            });
        let Some(display_name) = unique_image_name(
            &base_name,
            self.draft_images
                .iter()
                .map(|image| image.upload.display_name.as_str()),
        ) else {
            self.set_status("A unique bounded image name could not be assigned");
            return;
        };
        let Some(marker_start) = self.prompt.push_image_marker(&display_name) else {
            self.set_status("The image marker does not fit in the current message");
            return;
        };
        let normalized_bytes = image.bytes.len();
        self.draft_images.push(DraftImage {
            upload: morons_protocol::ImageUpload {
                display_name: display_name.clone(),
                marker_start,
                data_base64: morons_image::encode_base64(&image.bytes),
            },
            normalized_bytes,
        });
        self.reset_skill_completion();
        self.set_status(format!(
            "Attached [{display_name}] · {}×{} · {} bytes",
            image.width, image.height, normalized_bytes
        ));
    }

    pub(super) fn backspace_prompt(&mut self) {
        if let Some(image) = self.draft_images.last() {
            let marker_start = usize::try_from(image.upload.marker_start).unwrap_or(usize::MAX);
            let marker_end = marker_start.saturating_add(image.upload.display_name.len() + 2);
            if marker_end == self.prompt.len_bytes() && self.prompt.truncate(marker_start) {
                self.draft_images.pop();
                self.reset_skill_completion();
                return;
            }
        }
        self.prompt.backspace();
        self.reset_skill_completion();
    }

    fn image_uploads(&self) -> Vec<morons_protocol::ImageUpload> {
        self.draft_images
            .iter()
            .map(|image| image.upload.clone())
            .collect()
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
        mark_shared_directories(&mut self.sessions);
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

    pub(super) fn rename_session_applied(
        &mut self,
        session: SessionSummary,
    ) -> Result<(), UiStateError> {
        self.clear_pending();
        self.rename_dialog = None;
        self.apply_event(ApplicationEvent::SessionChanged {
            cursor: morons_protocol::SessionCatalogEventCursor::beginning(),
            session,
        })
    }

    pub(super) fn session_archive_applied(
        &mut self,
        session: SessionSummary,
    ) -> Result<(), UiStateError> {
        self.clear_pending();
        self.apply_event(ApplicationEvent::SessionChanged {
            cursor: morons_protocol::SessionCatalogEventCursor::beginning(),
            session,
        })
    }

    pub(super) fn session_deleted(&mut self, session_id: SessionId) -> Result<(), UiStateError> {
        if self.confirm_delete == Some(session_id) {
            self.confirm_delete = None;
        }
        if let Some(index) = self
            .sessions
            .iter()
            .position(|session| session.summary.id == session_id)
        {
            self.sessions.remove(index);
        }
        self.selected_session = self
            .selected_session
            .min(self.sessions.len().saturating_sub(1));
        mark_shared_directories(&mut self.sessions);
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.summary.id == session_id)
        {
            self.close_session();
        }
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
        mark_shared_directories(&mut self.sessions);
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
        let selected = self.selected_model().map(|model| OpenCodeModelSelection {
            service: model.model.service,
            model_id: model.model.id.clone(),
        });
        self.models.retain(|model| model.model.service != service);
        self.models
            .extend(models.into_iter().map(PresentedModel::new));
        self.loaded_model_catalogs[model_catalog_index(service)] = true;
        self.selected_model = self
            .default_model
            .as_ref()
            .and_then(|selection| self.available_model_index(selection))
            .or_else(|| {
                selected
                    .as_ref()
                    .and_then(|selection| self.available_model_index(selection))
            })
            .or_else(|| self.models.iter().position(|model| model.model.available));
        let dialog_matches = self.model_dialog_matches().len();
        if let Some(dialog) = self.model_dialog.as_mut() {
            dialog.selected = dialog.selected.min(dialog_matches.saturating_sub(1));
        }
        Ok(())
    }

    pub(super) fn install_default_model(&mut self, selection: Option<OpenCodeModelSelection>) {
        self.default_model = selection;
        if let Some(index) = self
            .default_model
            .as_ref()
            .and_then(|selection| self.available_model_index(selection))
        {
            self.selected_model = Some(index);
        } else if self
            .selected_model()
            .is_none_or(|model| !model.model.available)
        {
            self.selected_model = self.models.iter().position(|model| model.model.available);
        }
    }

    pub(super) fn default_model_is_unavailable(&self) -> bool {
        self.default_model.as_ref().is_some_and(|selection| {
            self.loaded_model_catalogs[model_catalog_index(selection.service)]
                && self.available_model_index(selection).is_none()
        })
    }

    pub(super) fn install_settings(&mut self, settings: ApplicationSettings) {
        self.settings = Some(settings);
    }

    pub(super) fn open_settings_dialog(&mut self) {
        self.settings_dialog = Some(SettingsDialog::Overview);
        self.set_status("Application settings are server-authoritative and global");
    }

    pub(super) fn open_subagent_model_dialog(&mut self) {
        self.settings_dialog = Some(SettingsDialog::SubagentModel {
            query: PromptBuffer::default(),
            selected: 0,
        });
        let matches = self.subagent_model_dialog_matches();
        let current = self
            .settings
            .as_ref()
            .map(|settings| &settings.subagent_model);
        if let Some(position) = matches
            .iter()
            .position(|candidate| match (candidate, current) {
                (
                    SubagentModelCandidate::InheritParent,
                    Some(SubagentModelSetting::InheritParent {}),
                ) => true,
                (
                    SubagentModelCandidate::Model(index),
                    Some(SubagentModelSetting::OpenCode { service, model_id }),
                ) => self.models.get(*index).is_some_and(|model| {
                    model.model.service == *service && model.model.id == *model_id
                }),
                _ => false,
            })
            && let Some(SettingsDialog::SubagentModel { selected, .. }) =
                self.settings_dialog.as_mut()
        {
            *selected = position;
        }
        self.set_status("Choose Inherit parent or one exact available reviewed subagent model");
    }

    pub(super) fn subagent_model_dialog_matches(&self) -> Vec<SubagentModelCandidate> {
        let query = match self.settings_dialog.as_ref() {
            Some(SettingsDialog::SubagentModel { query, .. }) => query.as_str(),
            _ => "",
        };
        let normalized = query.trim().to_lowercase();
        let mut matches = Vec::new();
        if normalized.is_empty()
            || "inherit parent default"
                .split_whitespace()
                .any(|label| label.contains(&normalized) || normalized.contains(label))
        {
            matches.push(SubagentModelCandidate::InheritParent);
        }
        let mut models = self
            .models
            .iter()
            .enumerate()
            .filter(|(_, model)| {
                model.model.available
                    && model.model.capabilities.text_input
                    && model.model.capabilities.text_output
                    && model.model.capabilities.tool_calls
            })
            .filter_map(|(index, model)| {
                model_search_score(model, query).map(|score| (score, index))
            })
            .collect::<Vec<_>>();
        if !normalized.is_empty() {
            models.sort_by_key(|(score, index)| (*score, *index));
        }
        matches.extend(
            models
                .into_iter()
                .map(|(_, index)| SubagentModelCandidate::Model(index)),
        );
        matches
    }

    pub(super) fn open_model_dialog(&mut self, initial_query: &str) {
        let mut query = PromptBuffer::default();
        query.push_paste(initial_query);
        let mut maximum = query.len_bytes().min(MAX_MODEL_SEARCH_BYTES);
        while !query.as_str().is_char_boundary(maximum) {
            maximum = maximum.saturating_sub(1);
        }
        let _ = query.truncate(maximum);
        self.model_dialog = Some(ModelDialog { query, selected: 0 });
        let matches = self.model_dialog_matches();
        if matches.is_empty() {
            self.set_status("No available reviewed models match this search");
            return;
        }
        if initial_query.is_empty()
            && let Some(selected_model) = self.selected_model
            && let Some(position) = matches.iter().position(|index| *index == selected_model)
            && let Some(dialog) = self.model_dialog.as_mut()
        {
            dialog.selected = position;
        }
        self.set_status("Search reviewed models; Enter saves the global default");
    }

    pub(super) fn model_dialog_matches(&self) -> Vec<usize> {
        let query = self
            .model_dialog
            .as_ref()
            .map(|dialog| dialog.query.as_str())
            .unwrap_or_default();
        let mut matches = self
            .models
            .iter()
            .enumerate()
            .filter(|(_, model)| model.model.available)
            .filter_map(|(index, model)| {
                model_search_score(model, query).map(|score| (score, index))
            })
            .collect::<Vec<_>>();
        if !query.trim().is_empty() {
            matches.sort_by_key(|(score, index)| {
                (
                    *score,
                    usize::from(Some(*index) != self.selected_model),
                    *index,
                )
            });
        }
        matches.into_iter().map(|(_, index)| index).collect()
    }

    fn available_model_index(&self, selection: &OpenCodeModelSelection) -> Option<usize> {
        self.models.iter().position(|model| {
            model.model.available
                && model.model.service == selection.service
                && model.model.id == selection.model_id
        })
    }

    #[cfg(test)]
    pub(super) fn open_session(
        &mut self,
        summary: SessionSummary,
        entries: Vec<TranscriptEntry>,
        runs: Vec<RunSummary>,
        active_run_id: Option<RunId>,
        active_command_id: Option<LocalCommandId>,
    ) -> Result<(), UiStateError> {
        self.open_session_window(TranscriptWindowData {
            summary,
            entries,
            runs,
            active_run_id,
            active_command_id,
            older_cursor: None,
            newer_cursor: None,
        })
    }

    pub(super) fn open_session_window(
        &mut self,
        window: TranscriptWindowData,
    ) -> Result<(), UiStateError> {
        let shared_directory =
            window
                .summary
                .working_directory
                .as_deref()
                .is_some_and(|directory| {
                    self.sessions
                        .iter()
                        .filter(|session| {
                            session.summary.working_directory.as_deref() == Some(directory)
                        })
                        .count()
                        > 1
                });
        let mut session = SessionView::new(window, Vec::new())?;
        session.shared_directory = shared_directory;
        self.session = Some(session);
        self.view = View::Session;
        self.prompt.clear();
        self.draft_images.clear();
        self.transcript_viewport.reset();
        self.transcript_page_loading = false;
        self.skill_completion_index = 0;
        Ok(())
    }

    pub(super) fn install_session_skills(
        &mut self,
        session_id: SessionId,
        skills: Vec<SkillSummary>,
    ) -> Result<(), UiStateError> {
        let session = self.session_mut(session_id)?;
        session.skills = skills.into_iter().map(PresentedSkill::new).collect();
        self.reset_skill_completion();
        Ok(())
    }

    pub(super) fn close_session(&mut self) {
        self.view = View::Sessions;
        self.session = None;
        self.model_dialog = None;
        self.settings_dialog = None;
        self.prompt.clear();
        self.draft_images.clear();
        self.pending = None;
        self.pending_unknown = false;
        self.transcript_viewport.reset();
        self.transcript_page_loading = false;
        self.skill_completion_index = 0;
    }

    pub(super) fn install_transcript_window(
        &mut self,
        mut window: TranscriptWindowData,
        navigation: TranscriptNavigation,
    ) -> Result<(), UiStateError> {
        let current = self
            .session
            .as_mut()
            .filter(|session| session.summary.id == window.summary.id)
            .ok_or(UiStateError::ResourceScopeMismatch)?;
        if navigation != TranscriptNavigation::Latest {
            for run in &mut window.runs {
                if let Some(live) = current.runs.iter().find(|live| live.id == run.id) {
                    *run = live.clone();
                }
            }
            if let Some(active_run_id) = current.active_run_id
                && !window.runs.iter().any(|run| run.id == active_run_id)
                && let Some(active) = current.runs.iter().find(|run| run.id == active_run_id)
            {
                window.runs.push(active.clone());
            }
            window.active_run_id = current.active_run_id;
            window.active_command_id = current.active_command_id;
        }
        let deferred_newer_output = current.deferred_newer_output;
        let transient = if navigation == TranscriptNavigation::Latest {
            current.transient.take()
        } else {
            None
        };
        window.summary = current.summary.clone();
        let mut replacement = SessionView::new(window, Vec::new())?;
        replacement.skills = std::mem::take(&mut current.skills);
        replacement.context_status = current.context_status.take();
        replacement.shared_directory = current.shared_directory;
        replacement.transient = transient.filter(|transient| {
            !replacement
                .entries
                .iter()
                .any(|entry| entry.role == "Assistant" && entry.run_id == Some(transient.run_id))
        });
        replacement.deferred_newer_output =
            navigation != TranscriptNavigation::Latest && deferred_newer_output;
        *current = replacement;
        self.transcript_page_loading = false;
        match navigation {
            TranscriptNavigation::Latest | TranscriptNavigation::Older(_) => {
                self.transcript_viewport.reset();
            }
            TranscriptNavigation::Oldest | TranscriptNavigation::Newer(_) => {
                self.transcript_viewport.reset_to_top();
            }
        }
        Ok(())
    }

    pub(super) fn transcript_page_failed(&mut self) {
        self.transcript_page_loading = false;
    }

    pub(super) fn requires_tail_refresh(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.tail_refresh_required)
            && !self.transcript_page_loading
    }

    pub(super) fn request_tail_refresh(&mut self) -> AppAction {
        let Some(session_id) = self.session.as_ref().map(|session| session.summary.id) else {
            return AppAction::None;
        };
        self.transcript_page_loading = true;
        AppAction::NavigateTranscript {
            session_id,
            navigation: TranscriptNavigation::Latest,
        }
    }

    pub(super) fn apply_event(&mut self, event: ApplicationEvent) -> Result<(), UiStateError> {
        match event {
            ApplicationEvent::SessionCreated { session, .. } => self.add_session(session),
            ApplicationEvent::SessionChanged { session, .. } => {
                let existing = self
                    .sessions
                    .iter_mut()
                    .find(|existing| existing.summary.id == session.id)
                    .ok_or(UiStateError::ResourceScopeMismatch)?;
                *existing = PresentedSession::new(session.clone());
                mark_shared_directories(&mut self.sessions);
                if let Some(open) = self
                    .session
                    .as_mut()
                    .filter(|open| open.summary.id == session.id)
                {
                    open.display_name = SafeText::from_untrusted(
                        session
                            .display_name
                            .as_deref()
                            .unwrap_or("Untitled session"),
                    );
                    open.summary = session;
                }
                Ok(())
            }
            ApplicationEvent::SessionRemoved { session_id, .. } => self.session_deleted(session_id),
            ApplicationEvent::SessionTranscriptEntryCommitted {
                session_id, entry, ..
            } => {
                let session = self.session_mut(session_id)?;
                if session.is_historical_window() {
                    session.defer_transcript_entry(&entry);
                    self.transcript_viewport.note_newer_output();
                } else if session.append_transcript_entry(entry)? {
                    self.transcript_viewport.note_content_changed();
                }
                Ok(())
            }
            ApplicationEvent::SessionRunChanged { run, .. } => {
                let presentation = terminal_run_presentation(&run);
                let status = presentation.map(|presentation| {
                    format!(
                        "{} · {} · {}: {}",
                        presentation.heading,
                        service_label(run.service),
                        run.model_id,
                        presentation.detail
                    )
                });
                let session = self.session_mut(run.session_id)?;
                session.context_status = None;
                let historical = session.is_historical_window();
                let had_transient = session.transient.is_some();
                session.apply_run(run)?;
                if historical {
                    session.deferred_newer_output = true;
                    self.transcript_viewport.note_newer_output();
                } else if presentation.is_some() {
                    self.transcript_viewport.note_content_changed();
                } else if had_transient && session.transient.is_none() {
                    self.transcript_viewport.note_layout_changed();
                }
                if let Some(status) = status {
                    self.set_status(status);
                }
                Ok(())
            }
            ApplicationEvent::SessionLocalCommandChanged {
                session_id,
                command_id,
                active,
                ..
            } => {
                let session = self.session_mut(session_id)?;
                session.active_command_id = active.then_some(command_id);
                Ok(())
            }
            ApplicationEvent::SessionAssistantDelta {
                session_id,
                run_id,
                delta,
                refusal,
                ..
            } => {
                let session = self.session_mut(session_id)?;
                if session.is_historical_window() {
                    session.deferred_newer_output = true;
                    self.transcript_viewport.note_newer_output();
                } else {
                    session.append_delta(run_id, &delta, refusal)?;
                    self.transcript_viewport.note_content_changed();
                }
                Ok(())
            }
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
        self.install_default_model(Some(OpenCodeModelSelection {
            service: run.service,
            model_id: run.model_id.clone(),
        }));
        self.prompt.clear();
        self.draft_images.clear();
        self.reset_skill_completion();
        self.clear_pending();
        self.transcript_viewport.scroll_to_bottom();
        let session = self.session_mut(run.session_id)?;
        session.context_status = None;
        session.apply_run(run)
    }

    pub(super) fn local_command_accepted(
        &mut self,
        session_id: SessionId,
        command_id: LocalCommandId,
    ) -> Result<(), UiStateError> {
        self.prompt.clear();
        self.reset_skill_completion();
        self.clear_pending();
        self.transcript_viewport.scroll_to_bottom();
        let session = self.session_mut(session_id)?;
        if !session
            .entries
            .iter()
            .any(|entry| entry.command_id == Some(command_id))
        {
            session.active_command_id = Some(command_id);
        }
        Ok(())
    }

    pub(super) fn local_command_cancellation_resolved(
        &mut self,
        command_id: LocalCommandId,
    ) -> Result<(), UiStateError> {
        self.clear_pending();
        let session = self
            .session
            .as_ref()
            .ok_or(UiStateError::ResourceScopeMismatch)?;
        if session.active_command_id != Some(command_id) {
            return Err(UiStateError::ResourceScopeMismatch);
        }
        Ok(())
    }

    pub(super) fn context_status_loaded(
        &mut self,
        context: SessionContextStatus,
    ) -> Result<(), UiStateError> {
        self.session_mut(context.session_id)?
            .install_context_status(context)?;
        if self.prompt.as_str() == "/context" {
            self.prompt.clear();
            self.information_dialog = Some(InformationDialog::Context);
        }
        Ok(())
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
            if session.transient.take().is_some() {
                self.transcript_viewport.note_layout_changed();
            }
        }
        Ok(())
    }

    pub(super) fn clear_transient_assistant(&mut self) {
        if self
            .session
            .as_mut()
            .and_then(|session| session.transient.take())
            .is_some()
        {
            self.transcript_viewport.note_layout_changed();
        }
    }

    pub(super) fn selected_session_id(&self) -> Option<SessionId> {
        self.sessions
            .get(self.selected_session)
            .map(|session| session.summary.id)
    }

    pub(super) fn selected_directory_is_shared(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.shared_directory)
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
    pub(super) working_directory: SafeText,
    pub(super) shared_directory: bool,
}

impl PresentedSession {
    fn new(summary: SessionSummary) -> Self {
        let display_name = SafeText::from_untrusted(
            summary
                .display_name
                .as_deref()
                .unwrap_or("Untitled session"),
        );
        let working_directory = SafeText::from_untrusted(
            summary
                .working_directory
                .as_deref()
                .unwrap_or("Legacy session · no direct working directory"),
        );
        Self {
            summary,
            display_name,
            working_directory,
            shared_directory: false,
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

pub(super) struct PresentedSkill {
    pub(super) name: String,
    pub(super) safe_name: SafeText,
    pub(super) description: SafeText,
    pub(super) source: morons_protocol::SkillSource,
}

impl PresentedSkill {
    fn new(skill: SkillSummary) -> Self {
        Self {
            safe_name: SafeText::from_untrusted(&skill.name),
            description: SafeText::from_untrusted(&skill.description),
            name: skill.name,
            source: skill.source,
        }
    }
}

pub(super) struct SessionView {
    pub(super) summary: SessionSummary,
    pub(super) display_name: SafeText,
    pub(super) entries: Vec<PresentedTranscriptEntry>,
    pub(super) runs: Vec<RunSummary>,
    pub(super) active_run_id: Option<RunId>,
    pub(super) active_command_id: Option<LocalCommandId>,
    pub(super) older_cursor: Option<TranscriptCursor>,
    pub(super) newer_cursor: Option<TranscriptCursor>,
    deferred_newer_output: bool,
    tail_refresh_required: bool,
    pub(super) skills: Vec<PresentedSkill>,
    pub(super) transient: Option<TransientAssistant>,
    pub(super) context_status: Option<SessionContextStatus>,
    pub(super) shared_directory: bool,
}

impl SessionView {
    fn new(window: TranscriptWindowData, skills: Vec<SkillSummary>) -> Result<Self, UiStateError> {
        let TranscriptWindowData {
            summary,
            entries,
            runs,
            active_run_id,
            active_command_id,
            older_cursor,
            newer_cursor,
        } = window;
        if entries.len() > MAX_CLIENT_TRANSCRIPT_ENTRIES || runs.len() > MAX_CLIENT_RUNS {
            return Err(UiStateError::ResourceLimitExceeded);
        }
        if runs.iter().any(|run| run.session_id != summary.id)
            || [older_cursor.as_ref(), newer_cursor.as_ref()]
                .iter()
                .flatten()
                .any(|cursor| cursor.as_bytes()[..16] != summary.id.as_bytes()[..])
            || entries.iter().any(|entry| {
                transcript_entry_run_id(entry)
                    .is_some_and(|run_id| !runs.iter().any(|run| run.id == run_id))
            })
            || active_run_id.is_some_and(|active_run_id| {
                !runs
                    .iter()
                    .any(|run| run.id == active_run_id && !run.state.is_terminal())
            })
            || (active_run_id.is_some() && active_command_id.is_some())
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
            display_name,
            entries: entries
                .into_iter()
                .map(PresentedTranscriptEntry::new)
                .collect(),
            runs,
            active_run_id,
            active_command_id,
            older_cursor,
            newer_cursor,
            deferred_newer_output: false,
            tail_refresh_required: false,
            skills: skills.into_iter().map(PresentedSkill::new).collect(),
            transient: None,
            context_status: None,
            shared_directory: false,
        })
    }

    fn is_historical_window(&self) -> bool {
        self.newer_cursor.is_some() || self.deferred_newer_output
    }

    fn defer_transcript_entry(&mut self, entry: &TranscriptEntry) {
        if let TranscriptEntry::LocalCommand { command_id, .. } = entry
            && self.active_command_id == Some(*command_id)
        {
            self.active_command_id = None;
        }
        if let TranscriptEntry::AssistantMessage { run_id, .. } = entry
            && self
                .transient
                .as_ref()
                .is_some_and(|transient| transient.run_id == *run_id)
        {
            self.transient = None;
        }
        self.deferred_newer_output = true;
    }

    fn append_transcript_entry(&mut self, entry: TranscriptEntry) -> Result<bool, UiStateError> {
        let id = transcript_entry_id(&entry);
        if self.entries.iter().any(|existing| existing.id == id) {
            return Ok(false);
        }
        if self.entries.len() >= MAX_CLIENT_TRANSCRIPT_ENTRIES {
            self.entries
                .drain(..TRANSCRIPT_WINDOW_TARGET_ENTRIES.min(self.entries.len()));
            self.older_cursor = None;
            self.tail_refresh_required = true;
        }
        let run_id = transcript_entry_run_id(&entry);
        if matches!(entry, TranscriptEntry::AssistantMessage { .. })
            && self
                .transient
                .as_ref()
                .is_some_and(|transient| Some(transient.run_id) == run_id)
        {
            self.transient = None;
        }
        if let TranscriptEntry::LocalCommand { command_id, .. } = &entry
            && self.active_command_id == Some(*command_id)
        {
            self.active_command_id = None;
        }
        self.entries.push(PresentedTranscriptEntry::new(entry));
        Ok(true)
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
            if self.is_historical_window()
                && !self
                    .entries
                    .iter()
                    .any(|entry| entry.run_id == Some(run.id))
            {
                self.runs.retain(|existing| existing.id != run.id);
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

    fn install_context_status(
        &mut self,
        context: SessionContextStatus,
    ) -> Result<(), UiStateError> {
        if context.session_id != self.summary.id {
            return Err(UiStateError::ResourceScopeMismatch);
        }
        self.context_status = Some(context);
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
    pub(super) run_id: Option<RunId>,
    command_id: Option<LocalCommandId>,
    pub(super) role: &'static str,
    pub(super) text: SafeText,
    pub(super) refusal: bool,
}

impl PresentedTranscriptEntry {
    fn new(entry: TranscriptEntry) -> Self {
        match entry {
            TranscriptEntry::UserMessage {
                id, run_id, text, ..
            } => Self {
                id,
                run_id: Some(run_id),
                command_id: None,
                role: "You",
                text: SafeText::from_untrusted(&text),
                refusal: false,
            },
            TranscriptEntry::AssistantMessage {
                id,
                run_id,
                text,
                refusal,
                ..
            } => Self {
                id,
                run_id: Some(run_id),
                command_id: None,
                role: "Assistant",
                text: SafeText::from_untrusted(&text),
                refusal,
            },
            TranscriptEntry::ToolCall {
                id,
                run_id,
                tool,
                path,
                ..
            } => Self {
                id,
                run_id: Some(run_id),
                command_id: None,
                role: "Tool call",
                text: SafeText::from_untrusted(&format!("{} · {path}", tool_label(tool))),
                refusal: false,
            },
            TranscriptEntry::ToolResult {
                id,
                run_id,
                tool,
                status,
                summary,
                ..
            } => Self {
                id,
                run_id: Some(run_id),
                command_id: None,
                role: "Tool result",
                text: SafeText::from_untrusted(&format!(
                    "{} · {status:?} · {summary}",
                    tool_label(tool)
                )),
                refusal: false,
            },
            TranscriptEntry::LocalCommand {
                id,
                command_id,
                command,
                context_visible,
                status,
                exit_code,
                signal,
                stdout,
                stderr,
                ..
            } => Self {
                id,
                run_id: None,
                command_id: Some(command_id),
                role: if context_visible {
                    "Command !"
                } else {
                    "Command !!"
                },
                text: SafeText::from_untrusted(&format!(
                    "{status:?} · exit {exit_code:?} · signal {signal:?}\n$ {command}\nstdout:\n{stdout}\nstderr:\n{stderr}"
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

fn mark_shared_directories(sessions: &mut [PresentedSession]) {
    for index in 0..sessions.len() {
        sessions[index].shared_directory = sessions[index]
            .summary
            .working_directory
            .as_deref()
            .is_some_and(|directory| {
                sessions.iter().enumerate().any(|(other_index, other)| {
                    other_index != index
                        && other.summary.working_directory.as_deref() == Some(directory)
                })
            });
    }
}

const fn initial_information_dialog() -> Option<InformationDialog> {
    #[cfg(test)]
    {
        None
    }
    #[cfg(not(test))]
    {
        Some(InformationDialog::TrustNotice)
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
        | TranscriptEntry::ToolResult { id, .. }
        | TranscriptEntry::LocalCommand { id, .. } => *id,
    }
}

fn transcript_entry_run_id(entry: &TranscriptEntry) -> Option<RunId> {
    match entry {
        TranscriptEntry::UserMessage { run_id, .. }
        | TranscriptEntry::AssistantMessage { run_id, .. }
        | TranscriptEntry::ToolCall { run_id, .. }
        | TranscriptEntry::ToolResult { run_id, .. } => Some(*run_id),
        TranscriptEntry::LocalCommand { .. } => None,
    }
}

fn sanitize_image_name(value: &str) -> String {
    let mut name = value
        .chars()
        .map(|character| {
            if character.is_control()
                || is_bidirectional_control(character)
                || matches!(character, '/' | '\\' | '[' | ']')
            {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();
    while name.len() > MAX_IMAGE_DISPLAY_NAME_BYTES {
        name.pop();
    }
    if matches!(name.as_str(), "" | "." | "..") {
        String::new()
    } else {
        name
    }
}

fn unique_image_name<'a>(base: &str, existing: impl Iterator<Item = &'a str>) -> Option<String> {
    let existing = existing.collect::<std::collections::BTreeSet<_>>();
    if !existing.contains(base) {
        return Some(base.to_owned());
    }
    let (stem, extension) = base
        .rsplit_once('.')
        .map_or((base, None), |(stem, extension)| (stem, Some(extension)));
    for index in 2_u32..=5 {
        let suffix = format!(" ({index})");
        let extension_bytes = extension.map_or(0, |extension| extension.len() + 1);
        let maximum_stem = MAX_IMAGE_DISPLAY_NAME_BYTES
            .saturating_sub(suffix.len())
            .saturating_sub(extension_bytes);
        let mut bounded_stem = stem.to_owned();
        while bounded_stem.len() > maximum_stem {
            bounded_stem.pop();
        }
        let candidate = sanitize_image_name(&extension.map_or_else(
            || format!("{bounded_stem}{suffix}"),
            |extension| format!("{bounded_stem}{suffix}.{extension}"),
        ));
        if !existing.contains(candidate.as_str()) {
            return Some(candidate);
        }
    }
    None
}

const fn tool_label(tool: morons_protocol::ToolKind) -> &'static str {
    match tool {
        morons_protocol::ToolKind::ListDirectory => "list_directory",
        morons_protocol::ToolKind::ReadFile => "read_file",
        morons_protocol::ToolKind::SearchText => "search_text",
        morons_protocol::ToolKind::EditFile => "edit_file",
        morons_protocol::ToolKind::CreateFile => "create_file",
        morons_protocol::ToolKind::CreateDirectory => "create_directory",
        morons_protocol::ToolKind::RunCommand => "run_command",
        morons_protocol::ToolKind::Read => "read",
        morons_protocol::ToolKind::Write => "write",
        morons_protocol::ToolKind::Edit => "edit",
        morons_protocol::ToolKind::Bash => "bash",
        morons_protocol::ToolKind::WebSearch => "web_search",
        morons_protocol::ToolKind::Ipython => "ipython",
        morons_protocol::ToolKind::Task => "task",
    }
}

#[cfg(test)]
mod tests;
