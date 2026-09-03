use std::{
    error::Error,
    fmt,
    path::{Component, Path},
};

use morons_protocol::ControlError;
use sha2::{Digest, Sha256};

use super::{
    paths::PathError,
    run_types::{
        MAX_MODEL_ID_BYTES, MAX_USER_MESSAGE_BYTES, RunId, RunModelSelection, RunOpenCodeService,
    },
};

pub(super) const IDENTIFIER_BYTES: usize = 16;
pub(super) const REQUEST_FINGERPRINT_BYTES: usize = 32;
pub(super) const MAX_SESSION_PAGE_SIZE: u16 = 100;
pub(super) const MAX_SESSION_CATALOG_EVENT_PAGE_SIZE: u16 = 100;
pub(super) const MAX_SESSION_EVENT_PAGE_SIZE: u16 = 100;
const MAX_SESSION_NAME_BYTES: usize = 256;
const CREATE_SESSION_FINGERPRINT_CONTEXT: &[u8] = b"morons.dev/create-session/v1\0";
const CREATE_SESSION_WITH_DIRECTORY_FINGERPRINT_CONTEXT: &[u8] = b"morons.dev/create-session/v2\0";
const RENAME_SESSION_FINGERPRINT_CONTEXT: &[u8] = b"morons.dev/rename-session/v1\0";
const ARCHIVE_SESSION_FINGERPRINT_CONTEXT: &[u8] = b"morons.dev/archive-session/v1\0";
const DELETE_SESSION_FINGERPRINT_CONTEXT: &[u8] = b"morons.dev/delete-session/v1\0";
const SUBMIT_SESSION_INPUT_FINGERPRINT_CONTEXT: &[u8] = b"morons.dev/submit-session-input/v1\0";
const SUBMIT_SESSION_INPUT_WITH_IMAGES_FINGERPRINT_CONTEXT: &[u8] =
    b"morons.dev/submit-session-input/v2\0";
const CANCEL_RUN_FINGERPRINT_CONTEXT: &[u8] = b"morons.dev/cancel-run/v1\0";
const STOP_SERVER_FINGERPRINT_CONTEXT: &[u8] = b"morons.dev/stop-server/v1\0";
const IMPORT_REPOSITORY_FINGERPRINT_CONTEXT: &[u8] = b"morons.dev/import-repository/v1\0";
const ACKNOWLEDGE_TOOL_UNCERTAINTY_CONTEXT: &[u8] = b"morons.dev/acknowledge-tool-uncertainty/v1\0";
const PROVISION_EXECUTION_IMAGE_CONTEXT: &[u8] = b"morons.dev/provision-execution-image/v1\0";

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId([u8; IDENTIFIER_BYTES]);

impl SessionId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; IDENTIFIER_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; IDENTIFIER_BYTES] {
        &self.0
    }
}

impl fmt::Debug for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SessionId(")?;
        write_hex(formatter, &self.0)?;
        formatter.write_str(")")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct MutationRequestId([u8; IDENTIFIER_BYTES]);

impl MutationRequestId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; IDENTIFIER_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; IDENTIFIER_BYTES] {
        &self.0
    }

    pub(super) const fn is_zero(self) -> bool {
        let mut index = 0;
        while index < self.0.len() {
            if self.0[index] != 0 {
                return false;
            }
            index += 1;
        }
        true
    }
}

impl fmt::Debug for MutationRequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MutationRequestId(")?;
        write_hex(formatter, &self.0)?;
        formatter.write_str(")")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionListCursor {
    snapshot_event_sequence: u64,
    after_created_sequence: u64,
}

impl SessionListCursor {
    #[must_use]
    pub const fn new(snapshot_event_sequence: u64, after_created_sequence: u64) -> Self {
        Self {
            snapshot_event_sequence,
            after_created_sequence,
        }
    }

    #[must_use]
    pub const fn snapshot_event_sequence(self) -> u64 {
        self.snapshot_event_sequence
    }

    #[must_use]
    pub const fn after_created_sequence(self) -> u64 {
        self.after_created_sequence
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionCatalogEventCursor(u64);

impl SessionCatalogEventCursor {
    #[must_use]
    pub const fn from_sequence(sequence: u64) -> Self {
        Self(sequence)
    }

    #[must_use]
    pub const fn sequence(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub id: SessionId,
    pub display_name: Option<String>,
    pub working_directory: Option<String>,
    pub archived: bool,
    pub created_sequence: u64,
    pub updated_sequence: u64,
    pub created_at_milliseconds: u64,
    pub(crate) workspace_id: [u8; IDENTIFIER_BYTES],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionPage {
    pub sessions: Vec<Session>,
    pub next_cursor: Option<SessionListCursor>,
    pub catalog_cursor: SessionCatalogEventCursor,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionCatalogEvent {
    pub cursor: SessionCatalogEventCursor,
    pub kind: SessionCatalogEventKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionCatalogEventKind {
    Created(Session),
    Changed(Session),
    Removed(SessionId),
}

impl SessionCatalogEvent {
    #[cfg(test)]
    pub fn session(&self) -> Option<&Session> {
        match &self.kind {
            SessionCatalogEventKind::Created(session)
            | SessionCatalogEventKind::Changed(session) => Some(session),
            SessionCatalogEventKind::Removed(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionCatalogEventPage {
    pub events: Vec<SessionCatalogEvent>,
    pub high_water: SessionCatalogEventCursor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerStopResult {
    pub signal_current_supervisor: bool,
    pub accepted_host_epoch: [u8; IDENTIFIER_BYTES],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenCodeCredentialStatus {
    pub configured: bool,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceState {
    Empty,
    Importing,
    Ready,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceBlockReason {
    InconsistentImportState,
    UncertainToolEffect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceSummary {
    pub state: WorkspaceState,
    pub file_count: u64,
    pub logical_bytes: u64,
    pub block_reason: Option<WorkspaceBlockReason>,
    pub blocked_run_id: Option<super::RunId>,
    pub blocked_tool: Option<crate::tools::ToolKind>,
}

/// Historical execution-image target retained only for durable schema validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecutionTargetOs {
    Macos,
    Linux,
    Windows,
}

impl ExecutionTargetOs {
    pub(crate) const fn from_record(value: i64) -> Option<Self> {
        match value {
            1 => Some(Self::Macos),
            2 => Some(Self::Linux),
            3 => Some(Self::Windows),
            _ => None,
        }
    }
}

/// Historical execution-image target retained only for durable schema validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecutionTargetArch {
    X86_64,
    Aarch64,
}

impl ExecutionTargetArch {
    pub(crate) const fn from_record(value: i64) -> Option<Self> {
        match value {
            1 => Some(Self::X86_64),
            2 => Some(Self::Aarch64),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceResourceLimit {
    Sessions,
    Runs,
    Context,
    Transcript,
    LogicalSequence,
    CredentialGeneration,
    CredentialMutations,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum PersistenceError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    Control(ControlError),
    Randomness(getrandom::Error),
    InvalidInput {
        reason: &'static str,
    },
    InvalidState {
        reason: &'static str,
    },
    RequestConflict,
    SessionNotFound,
    SessionArchived,
    SessionNotArchived,
    RunNotFound,
    LocalCommandNotFound,
    SessionBusy {
        active_run_id: RunId,
    },
    SessionCommandBusy {
        active_command_id: super::LocalCommandId,
    },
    WorkingDirectoryUnavailable,
    CredentialGenerationConflict,
    CredentialNotConfigured,
    CredentialMutationNotApplied,
    ImageInputUnsupported,
    WorkspaceBlocked,
    ResourceLimit {
        resource: PersistenceResourceLimit,
    },
    WorkerStopped,
}

impl fmt::Display for PersistenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "persistence I/O failed: {error}"),
            Self::Sqlite(error) => write!(formatter, "SQLite operation failed: {error}"),
            Self::Control(error) => write!(formatter, "persistence control failed: {error}"),
            Self::Randomness(error) => {
                write!(formatter, "persistence randomness failed: {error}")
            }
            Self::InvalidInput { reason } => {
                write!(formatter, "persistence input is invalid: {reason}")
            }
            Self::InvalidState { reason } => {
                write!(formatter, "persistent state is invalid: {reason}")
            }
            Self::RequestConflict => {
                formatter.write_str("the mutation request identifier conflicts with prior input")
            }
            Self::SessionNotFound => formatter.write_str("the session was not found"),
            Self::SessionArchived => formatter.write_str("the session is archived"),
            Self::SessionNotArchived => {
                formatter.write_str("the session must be archived before deletion")
            }
            Self::RunNotFound => formatter.write_str("the run was not found"),
            Self::LocalCommandNotFound => formatter.write_str("the local command was not found"),
            Self::SessionBusy { active_run_id } => {
                write!(formatter, "the session is busy with {active_run_id:?}")
            }
            Self::SessionCommandBusy { active_command_id } => {
                write!(formatter, "the session is busy with {active_command_id:?}")
            }
            Self::WorkingDirectoryUnavailable => {
                formatter.write_str("the session working directory is unavailable")
            }
            Self::CredentialGenerationConflict => {
                formatter.write_str("the credential generation changed")
            }
            Self::CredentialNotConfigured => {
                formatter.write_str("the OpenCode credential is not configured")
            }
            Self::CredentialMutationNotApplied => {
                formatter.write_str("the credential mutation was not applied")
            }
            Self::ImageInputUnsupported => {
                formatter.write_str("the selected model does not support image context")
            }
            Self::WorkspaceBlocked => formatter.write_str("the session workspace is blocked"),
            Self::ResourceLimit { resource } => match resource {
                PersistenceResourceLimit::Sessions => {
                    formatter.write_str("the persistence session count limit was reached")
                }
                PersistenceResourceLimit::Runs => {
                    formatter.write_str("the persistence run count limit was reached")
                }
                PersistenceResourceLimit::Context => {
                    formatter.write_str("the run context limit was reached")
                }
                PersistenceResourceLimit::Transcript => {
                    formatter.write_str("the session transcript limit was reached")
                }
                PersistenceResourceLimit::LogicalSequence => {
                    formatter.write_str("the persistence logical sequence limit was reached")
                }
                PersistenceResourceLimit::CredentialGeneration => {
                    formatter.write_str("the credential generation limit was reached")
                }
                PersistenceResourceLimit::CredentialMutations => {
                    formatter.write_str("the credential mutation limit was reached")
                }
            },
            Self::WorkerStopped => formatter.write_str("the persistence worker stopped"),
        }
    }
}

impl Error for PersistenceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::Control(error) => Some(error),
            Self::Randomness(error) => Some(error),
            Self::InvalidInput { .. }
            | Self::InvalidState { .. }
            | Self::RequestConflict
            | Self::SessionNotFound
            | Self::SessionArchived
            | Self::SessionNotArchived
            | Self::RunNotFound
            | Self::LocalCommandNotFound
            | Self::SessionBusy { .. }
            | Self::SessionCommandBusy { .. }
            | Self::WorkingDirectoryUnavailable
            | Self::CredentialGenerationConflict
            | Self::CredentialNotConfigured
            | Self::CredentialMutationNotApplied
            | Self::ImageInputUnsupported
            | Self::WorkspaceBlocked
            | Self::ResourceLimit { .. }
            | Self::WorkerStopped => None,
        }
    }
}

impl From<std::io::Error> for PersistenceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for PersistenceError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<ControlError> for PersistenceError {
    fn from(error: ControlError) -> Self {
        Self::Control(error)
    }
}

impl From<PathError> for PersistenceError {
    fn from(error: PathError) -> Self {
        match error {
            PathError::Io(error) => Self::Io(error),
            PathError::InvalidState { reason } => Self::InvalidState { reason },
        }
    }
}

impl From<getrandom::Error> for PersistenceError {
    fn from(error: getrandom::Error) -> Self {
        Self::Randomness(error)
    }
}

pub(super) fn validate_display_name(display_name: Option<&str>) -> Result<(), PersistenceError> {
    let Some(display_name) = display_name else {
        return Ok(());
    };
    if display_name.is_empty() || display_name.len() > MAX_SESSION_NAME_BYTES {
        return Err(PersistenceError::InvalidInput {
            reason: "a session display name must contain between 1 and 256 UTF-8 bytes",
        });
    }
    if display_name.chars().any(char::is_control) {
        return Err(PersistenceError::InvalidInput {
            reason: "a session display name must not contain control characters",
        });
    }
    Ok(())
}

pub(super) fn validate_working_directory_path(path: &str) -> Result<(), PersistenceError> {
    if path.is_empty()
        || path.len() > morons_protocol::MAX_WORKING_DIRECTORY_PATH_BYTES
        || path.chars().any(char::is_control)
    {
        return Err(PersistenceError::InvalidInput {
            reason: "a session working directory path is invalid",
        });
    }
    let path = Path::new(path);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(PersistenceError::InvalidInput {
            reason: "a session working directory must be a normalized absolute path",
        });
    }
    Ok(())
}

pub(super) fn validate_user_text(text: &str) -> Result<(), PersistenceError> {
    if text.is_empty() || text.len() > MAX_USER_MESSAGE_BYTES {
        return Err(PersistenceError::InvalidInput {
            reason: "session input must contain between 1 and 65536 UTF-8 bytes",
        });
    }
    Ok(())
}

pub(super) fn validate_model_identifier(model_id: &str) -> Result<(), PersistenceError> {
    if model_id.is_empty()
        || model_id.len() > MAX_MODEL_ID_BYTES
        || !model_id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
    {
        return Err(PersistenceError::InvalidInput {
            reason: "the run model identifier is invalid",
        });
    }
    Ok(())
}

pub(super) fn validate_model_selection(
    selection: &RunModelSelection,
) -> Result<(), PersistenceError> {
    validate_model_identifier(&selection.model_id)?;
    if selection.protocol_revision == 0
        || selection.maximum_input_tokens == 0
        || selection.maximum_output_tokens == 0
    {
        return Err(PersistenceError::InvalidInput {
            reason: "the run model selection is invalid",
        });
    }
    Ok(())
}

pub(super) fn submit_session_input_fingerprint(
    session_id: SessionId,
    text: &str,
    service: RunOpenCodeService,
    model_id: &str,
) -> [u8; REQUEST_FINGERPRINT_BYTES] {
    let mut digest = Sha256::new();
    digest.update(SUBMIT_SESSION_INPUT_FINGERPRINT_CONTEXT);
    digest.update(session_id.as_bytes());
    digest.update((text.len() as u32).to_be_bytes());
    digest.update(text.as_bytes());
    digest.update([match service {
        RunOpenCodeService::Zen => 1,
        RunOpenCodeService::Go => 2,
    }]);
    digest.update((model_id.len() as u16).to_be_bytes());
    digest.update(model_id.as_bytes());
    digest.finalize().into()
}

pub(super) fn submit_session_input_with_images_fingerprint(
    session_id: SessionId,
    text: &str,
    service: RunOpenCodeService,
    model_id: &str,
    attachment_digest: &[u8; 32],
) -> [u8; REQUEST_FINGERPRINT_BYTES] {
    let base = submit_session_input_fingerprint(session_id, text, service, model_id);
    let mut digest = Sha256::new();
    digest.update(SUBMIT_SESSION_INPUT_WITH_IMAGES_FINGERPRINT_CONTEXT);
    digest.update(base);
    digest.update(attachment_digest);
    digest.finalize().into()
}

pub(super) fn acknowledge_tool_uncertainty_fingerprint(
    session_id: SessionId,
    run_id: RunId,
) -> [u8; REQUEST_FINGERPRINT_BYTES] {
    let mut digest = Sha256::new();
    digest.update(ACKNOWLEDGE_TOOL_UNCERTAINTY_CONTEXT);
    digest.update(session_id.as_bytes());
    digest.update(run_id.as_bytes());
    digest.finalize().into()
}

pub(super) fn cancel_run_fingerprint(
    session_id: SessionId,
    run_id: RunId,
) -> [u8; REQUEST_FINGERPRINT_BYTES] {
    let mut digest = Sha256::new();
    digest.update(CANCEL_RUN_FINGERPRINT_CONTEXT);
    digest.update(session_id.as_bytes());
    digest.update(run_id.as_bytes());
    digest.finalize().into()
}

pub(super) fn import_repository_fingerprint_from_digest(
    session_id: SessionId,
    source_path_digest: [u8; REQUEST_FINGERPRINT_BYTES],
) -> [u8; REQUEST_FINGERPRINT_BYTES] {
    let mut digest = Sha256::new();
    digest.update(IMPORT_REPOSITORY_FINGERPRINT_CONTEXT);
    digest.update(session_id.as_bytes());
    digest.update(source_path_digest);
    digest.finalize().into()
}

pub(super) fn provision_execution_image_fingerprint(
    toolchain_digest: [u8; REQUEST_FINGERPRINT_BYTES],
    cargo_digest: [u8; REQUEST_FINGERPRINT_BYTES],
) -> [u8; REQUEST_FINGERPRINT_BYTES] {
    let mut digest = Sha256::new();
    digest.update(PROVISION_EXECUTION_IMAGE_CONTEXT);
    digest.update(toolchain_digest);
    digest.update(cargo_digest);
    digest.finalize().into()
}

pub(super) fn stop_server_fingerprint() -> [u8; REQUEST_FINGERPRINT_BYTES] {
    Sha256::digest(STOP_SERVER_FINGERPRINT_CONTEXT).into()
}

pub(super) fn create_session_fingerprint(
    display_name: Option<&str>,
) -> [u8; REQUEST_FINGERPRINT_BYTES] {
    let mut digest = Sha256::new();
    digest.update(CREATE_SESSION_FINGERPRINT_CONTEXT);
    update_optional_name(&mut digest, display_name);
    digest.finalize().into()
}

pub(super) fn rename_session_fingerprint(
    session_id: SessionId,
    display_name: &str,
) -> [u8; REQUEST_FINGERPRINT_BYTES] {
    let mut digest = Sha256::new();
    digest.update(RENAME_SESSION_FINGERPRINT_CONTEXT);
    digest.update(session_id.as_bytes());
    digest.update((display_name.len() as u64).to_be_bytes());
    digest.update(display_name.as_bytes());
    digest.finalize().into()
}

pub(super) fn archive_session_fingerprint(
    session_id: SessionId,
    archived: bool,
) -> [u8; REQUEST_FINGERPRINT_BYTES] {
    let mut digest = Sha256::new();
    digest.update(ARCHIVE_SESSION_FINGERPRINT_CONTEXT);
    digest.update(session_id.as_bytes());
    digest.update([u8::from(archived)]);
    digest.finalize().into()
}

pub(super) fn delete_session_fingerprint(session_id: SessionId) -> [u8; REQUEST_FINGERPRINT_BYTES] {
    let mut digest = Sha256::new();
    digest.update(DELETE_SESSION_FINGERPRINT_CONTEXT);
    digest.update(session_id.as_bytes());
    digest.finalize().into()
}

pub(super) fn create_session_with_directory_fingerprint(
    display_name: Option<&str>,
    working_directory: &str,
) -> [u8; REQUEST_FINGERPRINT_BYTES] {
    let mut digest = Sha256::new();
    digest.update(CREATE_SESSION_WITH_DIRECTORY_FINGERPRINT_CONTEXT);
    update_optional_name(&mut digest, display_name);
    digest.update((working_directory.len() as u32).to_be_bytes());
    digest.update(working_directory.as_bytes());
    digest.finalize().into()
}

fn update_optional_name(digest: &mut Sha256, display_name: Option<&str>) {
    match display_name {
        Some(name) => {
            digest.update([1]);
            digest.update((name.len() as u32).to_be_bytes());
            digest.update(name.as_bytes());
        }
        None => digest.update([0]),
    }
}

fn write_hex(formatter: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    for byte in bytes {
        write!(formatter, "{byte:02x}")?;
    }
    Ok(())
}
