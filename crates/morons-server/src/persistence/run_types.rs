use std::fmt;

use super::{Session, SessionId, types::IDENTIFIER_BYTES};

pub const CONTEXT_POLICY_VERSION: u16 = 1;
pub(super) const MAX_USER_MESSAGE_BYTES: usize = 64 * 1024;
pub(crate) const MAX_TRANSCRIPT_TEXT_BYTES: usize = 128 * 1024;
pub(super) const MAX_MODEL_ID_BYTES: usize = 128;
pub(super) const MAX_TRANSCRIPT_PAGE_SIZE: u16 = 1;
pub(super) const MAX_CONTEXT_ENTRIES: usize = 256;
pub(super) const MAX_TRANSCRIPT_ENTRIES: u64 = 100_000;
const CONTEXT_ITEM_OVERHEAD_TOKENS: u64 = 16;

pub(super) fn conservative_input_token_estimate(text_bytes: u64, entry_count: u64) -> Option<u32> {
    text_bytes
        .checked_add(entry_count.checked_mul(CONTEXT_ITEM_OVERHEAD_TOKENS)?)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct RunId([u8; IDENTIFIER_BYTES]);

impl RunId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; IDENTIFIER_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; IDENTIFIER_BYTES] {
        &self.0
    }
}

impl fmt::Debug for RunId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RunId(")?;
        write_hex(formatter, &self.0)?;
        formatter.write_str(")")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct MessageId([u8; IDENTIFIER_BYTES]);

impl MessageId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; IDENTIFIER_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; IDENTIFIER_BYTES] {
        &self.0
    }
}

impl fmt::Debug for MessageId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MessageId(")?;
        write_hex(formatter, &self.0)?;
        formatter.write_str(")")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunOpenCodeService {
    Zen,
    Go,
}

impl RunOpenCodeService {
    pub(super) const fn to_record(self) -> i64 {
        match self {
            Self::Zen => 1,
            Self::Go => 2,
        }
    }

    pub(super) fn from_record(value: i64) -> rusqlite::Result<Self> {
        match value {
            1 => Ok(Self::Zen),
            2 => Ok(Self::Go),
            _ => Err(rusqlite::Error::InvalidColumnType(
                0,
                "open_code_service".to_owned(),
                rusqlite::types::Type::Integer,
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Accepted,
    Active,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

impl RunState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }

    pub(super) const fn to_record(self) -> i64 {
        match self {
            Self::Accepted => 1,
            Self::Active => 2,
            Self::Succeeded => 3,
            Self::Failed => 4,
            Self::Cancelled => 5,
            Self::Interrupted => 6,
        }
    }

    pub(super) fn from_record(value: i64) -> rusqlite::Result<Self> {
        match value {
            1 => Ok(Self::Accepted),
            2 => Ok(Self::Active),
            3 => Ok(Self::Succeeded),
            4 => Ok(Self::Failed),
            5 => Ok(Self::Cancelled),
            6 => Ok(Self::Interrupted),
            _ => Err(rusqlite::Error::InvalidColumnType(
                0,
                "run_state".to_owned(),
                rusqlite::types::Type::Integer,
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunFailureKind {
    CredentialChanged,
    CredentialNotConfigured,
    AuthenticationOrEntitlement,
    RateLimited,
    ProviderUnavailable,
    ProviderRejected,
    ProviderProtocol,
    InvalidProviderOutput,
    ResourceLimit,
    Internal,
}

impl RunFailureKind {
    pub(super) const fn to_record(self) -> i64 {
        match self {
            Self::CredentialChanged => 1,
            Self::CredentialNotConfigured => 2,
            Self::AuthenticationOrEntitlement => 3,
            Self::RateLimited => 4,
            Self::ProviderUnavailable => 5,
            Self::ProviderRejected => 6,
            Self::ProviderProtocol => 7,
            Self::InvalidProviderOutput => 8,
            Self::ResourceLimit => 9,
            Self::Internal => 10,
        }
    }

    pub(super) fn from_record(value: i64) -> rusqlite::Result<Self> {
        match value {
            1 => Ok(Self::CredentialChanged),
            2 => Ok(Self::CredentialNotConfigured),
            3 => Ok(Self::AuthenticationOrEntitlement),
            4 => Ok(Self::RateLimited),
            5 => Ok(Self::ProviderUnavailable),
            6 => Ok(Self::ProviderRejected),
            7 => Ok(Self::ProviderProtocol),
            8 => Ok(Self::InvalidProviderOutput),
            9 => Ok(Self::ResourceLimit),
            10 => Ok(Self::Internal),
            _ => Err(rusqlite::Error::InvalidColumnType(
                0,
                "run_failure_kind".to_owned(),
                rusqlite::types::Type::Integer,
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub id: RunId,
    pub session_id: SessionId,
    pub user_message_id: MessageId,
    pub service: RunOpenCodeService,
    pub model_id: String,
    pub protocol_revision: u16,
    pub credential_generation: u64,
    pub context_policy_version: u16,
    pub state: RunState,
    pub cancellation_requested: bool,
    pub failure: Option<RunFailureKind>,
    pub accepted_at_milliseconds: u64,
    pub updated_at_milliseconds: u64,
    pub(crate) source_entry_high_water: u64,
    pub(crate) estimated_input_tokens: u32,
    pub(crate) maximum_input_tokens: u32,
    pub(crate) maximum_output_tokens: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedRun {
    pub user_message_id: MessageId,
    pub run: Run,
    pub(crate) newly_accepted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunCancellationResult {
    pub run_id: RunId,
    pub state: RunState,
    pub cancellation_requested: bool,
    pub(crate) intent_applied: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TranscriptCursor {
    session_id: SessionId,
    snapshot_entry_sequence: u64,
    snapshot_event_sequence: u64,
    after_entry_sequence: u64,
}

impl TranscriptCursor {
    #[must_use]
    pub const fn new(
        session_id: SessionId,
        snapshot_entry_sequence: u64,
        snapshot_event_sequence: u64,
        after_entry_sequence: u64,
    ) -> Self {
        Self {
            session_id,
            snapshot_entry_sequence,
            snapshot_event_sequence,
            after_entry_sequence,
        }
    }

    #[must_use]
    pub const fn session_id(self) -> SessionId {
        self.session_id
    }

    #[must_use]
    pub const fn snapshot_entry_sequence(self) -> u64 {
        self.snapshot_entry_sequence
    }

    #[must_use]
    pub const fn snapshot_event_sequence(self) -> u64 {
        self.snapshot_event_sequence
    }

    #[must_use]
    pub const fn after_entry_sequence(self) -> u64 {
        self.after_entry_sequence
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum TranscriptEntry {
    UserMessage {
        entry_sequence: u64,
        id: MessageId,
        run_id: RunId,
        text: String,
        created_at_milliseconds: u64,
    },
    AssistantMessage {
        entry_sequence: u64,
        id: MessageId,
        run_id: RunId,
        service: RunOpenCodeService,
        model_id: String,
        text: String,
        refusal: bool,
        created_at_milliseconds: u64,
    },
}

impl TranscriptEntry {
    #[must_use]
    pub const fn entry_sequence(&self) -> u64 {
        match self {
            Self::UserMessage { entry_sequence, .. }
            | Self::AssistantMessage { entry_sequence, .. } => *entry_sequence,
        }
    }

    #[must_use]
    pub const fn run_id(&self) -> RunId {
        match self {
            Self::UserMessage { run_id, .. } | Self::AssistantMessage { run_id, .. } => *run_id,
        }
    }
}

impl fmt::Debug for TranscriptEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UserMessage {
                entry_sequence,
                id,
                run_id,
                text,
                created_at_milliseconds,
            } => formatter
                .debug_struct("UserMessage")
                .field("entry_sequence", entry_sequence)
                .field("id", id)
                .field("run_id", run_id)
                .field("text_bytes", &text.len())
                .field("created_at_milliseconds", created_at_milliseconds)
                .finish(),
            Self::AssistantMessage {
                entry_sequence,
                id,
                run_id,
                service,
                model_id,
                text,
                refusal,
                created_at_milliseconds,
            } => formatter
                .debug_struct("AssistantMessage")
                .field("entry_sequence", entry_sequence)
                .field("id", id)
                .field("run_id", run_id)
                .field("service", service)
                .field("model_id", model_id)
                .field("text_bytes", &text.len())
                .field("refusal", refusal)
                .field("created_at_milliseconds", created_at_milliseconds)
                .finish(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionEventCursor {
    session_id: SessionId,
    sequence: u64,
}

impl SessionEventCursor {
    #[must_use]
    pub const fn new(session_id: SessionId, sequence: u64) -> Self {
        Self {
            session_id,
            sequence,
        }
    }

    #[must_use]
    pub const fn session_id(self) -> SessionId {
        self.session_id
    }

    #[must_use]
    pub const fn sequence(self) -> u64 {
        self.sequence
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEventPayload {
    TranscriptEntry(TranscriptEntry),
    RunChanged(Run),
    WorkspaceChanged(super::types::WorkspaceSummary),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEvent {
    pub cursor: SessionEventCursor,
    pub payload: SessionEventPayload,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEventPage {
    pub events: Vec<SessionEvent>,
    pub high_water: SessionEventCursor,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptPage {
    pub session: Session,
    pub workspace: super::types::WorkspaceSummary,
    pub entries: Vec<TranscriptEntry>,
    pub runs: Vec<Run>,
    pub active_run_id: Option<RunId>,
    pub next_cursor: Option<TranscriptCursor>,
    pub event_cursor: SessionEventCursor,
}

#[derive(Clone, Debug)]
pub struct RunModelSelection {
    pub service: RunOpenCodeService,
    pub model_id: String,
    pub protocol_revision: u16,
    pub maximum_input_tokens: u32,
    pub maximum_output_tokens: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct RunContext {
    pub run: Run,
    pub entries: Vec<TranscriptEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActivationOutcome {
    Active,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProviderOperationId([u8; IDENTIFIER_BYTES]);

impl ProviderOperationId {
    pub(crate) const fn from_bytes(bytes: [u8; IDENTIFIER_BYTES]) -> Self {
        Self(bytes)
    }

    pub(crate) const fn as_bytes(&self) -> &[u8; IDENTIFIER_BYTES] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrepareOperationOutcome {
    Prepared(ProviderOperationId),
    Cancelled,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DispatchOutcome {
    Dispatched,
    Cancelled,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProviderUsage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_write_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Clone)]
pub(crate) struct CompletedAssistant {
    pub text: String,
    pub refusal: bool,
    pub provider_response_id: String,
    pub usage: ProviderUsage,
}

impl fmt::Debug for CompletedAssistant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompletedAssistant")
            .field("text_bytes", &self.text.len())
            .field("refusal", &self.refusal)
            .field("provider_response_id", &self.provider_response_id)
            .field("usage", &self.usage)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProviderOperationFailureState {
    Failed,
    Uncertain,
}

fn write_hex(formatter: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    for byte in bytes {
        write!(formatter, "{byte:02x}")?;
    }
    Ok(())
}
