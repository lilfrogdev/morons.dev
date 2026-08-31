use std::{error::Error, fmt};

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
const SUBMIT_SESSION_INPUT_FINGERPRINT_CONTEXT: &[u8] = b"morons.dev/submit-session-input/v1\0";
const CANCEL_RUN_FINGERPRINT_CONTEXT: &[u8] = b"morons.dev/cancel-run/v1\0";

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
    pub session: Session,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionCatalogEventPage {
    pub events: Vec<SessionCatalogEvent>,
    pub high_water: SessionCatalogEventCursor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenCodeCredentialStatus {
    pub configured: bool,
    pub generation: u64,
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
    InvalidInput { reason: &'static str },
    InvalidState { reason: &'static str },
    RequestConflict,
    SessionNotFound,
    RunNotFound,
    SessionBusy { active_run_id: RunId },
    CredentialGenerationConflict,
    CredentialNotConfigured,
    CredentialMutationNotApplied,
    ResourceLimit { resource: PersistenceResourceLimit },
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
            Self::RunNotFound => formatter.write_str("the run was not found"),
            Self::SessionBusy { active_run_id } => {
                write!(formatter, "the session is busy with {active_run_id:?}")
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
            | Self::RunNotFound
            | Self::SessionBusy { .. }
            | Self::CredentialGenerationConflict
            | Self::CredentialNotConfigured
            | Self::CredentialMutationNotApplied
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

pub(super) fn create_session_fingerprint(
    display_name: Option<&str>,
) -> [u8; REQUEST_FINGERPRINT_BYTES] {
    let mut digest = Sha256::new();
    digest.update(CREATE_SESSION_FINGERPRINT_CONTEXT);
    match display_name {
        Some(name) => {
            digest.update([1]);
            digest.update((name.len() as u32).to_be_bytes());
            digest.update(name.as_bytes());
        }
        None => digest.update([0]),
    }
    digest.finalize().into()
}

fn write_hex(formatter: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    for byte in bytes {
        write!(formatter, "{byte:02x}")?;
    }
    Ok(())
}
