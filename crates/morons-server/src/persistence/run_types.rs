use std::fmt;

use super::{Session, SessionId, types::IDENTIFIER_BYTES};
use crate::tools::{ToolInput, ToolKind, ToolResult, ValidatedProviderCall};

pub const CONTEXT_POLICY_VERSION: u16 = 4;
pub(super) const LEGACY_CONTEXT_POLICY_VERSION: u16 = 1;
pub(super) const LEGACY_SKILL_CONTEXT_POLICY_VERSION: u16 = 2;
pub(super) const LEGACY_IMAGE_CONTEXT_POLICY_VERSION: u16 = 3;
pub(super) const MAX_USER_MESSAGE_BYTES: usize = 64 * 1024;
pub(crate) const MAX_TRANSCRIPT_TEXT_BYTES: usize = 128 * 1024;
pub(super) const MAX_MODEL_ID_BYTES: usize = 128;
pub(super) const MAX_TRANSCRIPT_PAGE_SIZE: u16 = 1;
pub(super) const MAX_CONTEXT_ENTRIES: usize = 256;
pub(super) const MAX_TRANSCRIPT_ENTRIES: u64 = 100_000;
const CONTEXT_ITEM_OVERHEAD_TOKENS: u64 = 16;

pub(crate) fn conservative_input_token_estimate(text_bytes: u64, entry_count: u64) -> Option<u32> {
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
pub struct LocalCommandId([u8; IDENTIFIER_BYTES]);

impl LocalCommandId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; IDENTIFIER_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; IDENTIFIER_BYTES] {
        &self.0
    }
}

impl fmt::Debug for LocalCommandId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LocalCommandId(")?;
        write_hex(formatter, &self.0)?;
        formatter.write_str(")")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalCommandStatus {
    Succeeded,
    Failed,
    Interrupted,
    Uncertain,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedLocalCommand {
    pub id: LocalCommandId,
    pub session_id: SessionId,
    pub command: String,
    pub context_visible: bool,
    pub(crate) newly_accepted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalCommandCancellationResult {
    pub command_id: LocalCommandId,
    pub cancellation_requested: bool,
    pub(crate) intent_applied: bool,
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

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageAttachmentId([u8; IDENTIFIER_BYTES]);

impl ImageAttachmentId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; IDENTIFIER_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; IDENTIFIER_BYTES] {
        &self.0
    }
}

impl fmt::Debug for ImageAttachmentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ImageAttachmentId(")?;
        write_hex(formatter, &self.0)?;
        formatter.write_str(")")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PreparedImageAttachment {
    pub display_name: String,
    pub marker_start: u32,
    pub media_type: morons_image::ImageMediaType,
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
    pub digest: [u8; 32],
}

impl fmt::Debug for PreparedImageAttachment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedImageAttachment")
            .field("display_name_bytes", &self.display_name.len())
            .field("marker_start", &self.marker_start)
            .field("media_type", &self.media_type)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.bytes.len())
            .field("digest", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageAttachment {
    pub id: ImageAttachmentId,
    pub display_name: String,
    pub marker_start: u32,
    pub media_type: morons_image::ImageMediaType,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    pub digest: [u8; 32],
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ToolCallId([u8; IDENTIFIER_BYTES]);

impl ToolCallId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; IDENTIFIER_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; IDENTIFIER_BYTES] {
        &self.0
    }
}

impl fmt::Debug for ToolCallId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ToolCallId(")?;
        write_hex(formatter, &self.0)?;
        formatter.write_str(")")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunOpenCodeService {
    Zen,
    Go,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultModelSelection {
    pub service: RunOpenCodeService,
    pub model_id: String,
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
    Uncertain,
}

impl RunState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Interrupted | Self::Uncertain
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
            Self::Uncertain => 7,
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
            7 => Ok(Self::Uncertain),
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
    ToolExecution,
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
            Self::ToolExecution => 11,
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
            11 => Ok(Self::ToolExecution),
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
    pub tool_catalog_version: u16,
    pub tool_limits_version: u16,
    pub execution_image_generation: Option<[u8; 16]>,
    pub state: RunState,
    pub cancellation_requested: bool,
    pub failure: Option<RunFailureKind>,
    pub accepted_at_milliseconds: u64,
    pub updated_at_milliseconds: u64,
    pub(crate) source_entry_high_water: u64,
    pub(crate) estimated_input_tokens: u32,
    pub(crate) maximum_input_tokens: u32,
    pub(crate) maximum_output_tokens: u32,
    pub(crate) provider_turns: u16,
    pub(crate) tool_calls: u32,
    pub(crate) tool_mutations: u32,
    pub(crate) tool_result_bytes: u64,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AssistantMessagePhase {
    Commentary,
    Final,
}

#[derive(Clone, PartialEq, Eq)]
pub enum TranscriptEntry {
    UserMessage {
        entry_sequence: u64,
        id: MessageId,
        run_id: RunId,
        text: String,
        attachments: Vec<ImageAttachment>,
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
        phase: AssistantMessagePhase,
        created_at_milliseconds: u64,
    },
    ToolCall {
        entry_sequence: u64,
        id: MessageId,
        run_id: RunId,
        call_id: ToolCallId,
        operation_id: ToolOperationId,
        provider_operation_id: ProviderOperationId,
        input: ToolInput,
        created_at_milliseconds: u64,
    },
    ToolResult {
        entry_sequence: u64,
        id: MessageId,
        run_id: RunId,
        call_id: ToolCallId,
        operation_id: ToolOperationId,
        tool: ToolKind,
        result: ToolResult,
        created_at_milliseconds: u64,
    },
    LocalCommand {
        entry_sequence: u64,
        id: MessageId,
        command_id: LocalCommandId,
        command: String,
        context_visible: bool,
        status: LocalCommandStatus,
        exit_code: Option<i32>,
        signal: Option<u16>,
        stdout: String,
        stderr: String,
        created_at_milliseconds: u64,
    },
}

impl TranscriptEntry {
    #[must_use]
    pub const fn entry_sequence(&self) -> u64 {
        match self {
            Self::UserMessage { entry_sequence, .. }
            | Self::AssistantMessage { entry_sequence, .. }
            | Self::ToolCall { entry_sequence, .. }
            | Self::ToolResult { entry_sequence, .. }
            | Self::LocalCommand { entry_sequence, .. } => *entry_sequence,
        }
    }

    #[must_use]
    pub const fn run_id(&self) -> Option<RunId> {
        match self {
            Self::UserMessage { run_id, .. }
            | Self::AssistantMessage { run_id, .. }
            | Self::ToolCall { run_id, .. }
            | Self::ToolResult { run_id, .. } => Some(*run_id),
            Self::LocalCommand { .. } => None,
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
                attachments,
                created_at_milliseconds,
            } => formatter
                .debug_struct("UserMessage")
                .field("entry_sequence", entry_sequence)
                .field("id", id)
                .field("run_id", run_id)
                .field("text_bytes", &text.len())
                .field("attachments", &attachments.len())
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
                phase,
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
                .field("phase", phase)
                .field("created_at_milliseconds", created_at_milliseconds)
                .finish(),
            Self::ToolCall {
                entry_sequence,
                id,
                run_id,
                call_id,
                input,
                created_at_milliseconds,
                ..
            } => formatter
                .debug_struct("ToolCall")
                .field("entry_sequence", entry_sequence)
                .field("id", id)
                .field("run_id", run_id)
                .field("call_id", call_id)
                .field("tool", &input.kind())
                .field("path_bytes", &input.path_text().len())
                .field("created_at_milliseconds", created_at_milliseconds)
                .finish(),
            Self::ToolResult {
                entry_sequence,
                id,
                run_id,
                call_id,
                tool,
                result,
                created_at_milliseconds,
                ..
            } => formatter
                .debug_struct("ToolResult")
                .field("entry_sequence", entry_sequence)
                .field("id", id)
                .field("run_id", run_id)
                .field("call_id", call_id)
                .field("tool", tool)
                .field("uncertain", &result.is_uncertain())
                .field("created_at_milliseconds", created_at_milliseconds)
                .finish(),
            Self::LocalCommand {
                entry_sequence,
                id,
                command_id,
                command,
                context_visible,
                status,
                exit_code,
                signal,
                stdout,
                stderr,
                created_at_milliseconds,
            } => formatter
                .debug_struct("LocalCommand")
                .field("entry_sequence", entry_sequence)
                .field("id", id)
                .field("command_id", command_id)
                .field("command_bytes", &command.len())
                .field("context_visible", context_visible)
                .field("status", status)
                .field("exit_code", exit_code)
                .field("signal", signal)
                .field("stdout_bytes", &stdout.len())
                .field("stderr_bytes", &stderr.len())
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
    LocalCommandChanged {
        command_id: LocalCommandId,
        active: bool,
    },
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
    pub active_command_id: Option<LocalCommandId>,
    pub next_cursor: Option<TranscriptCursor>,
    pub event_cursor: SessionEventCursor,
}

#[derive(Clone, Debug)]
pub(crate) struct RunInputContext {
    pub skills: crate::skills::RunSkillContext,
    pub attachments: Vec<PreparedImageAttachment>,
}

#[derive(Clone, Debug)]
pub struct RunModelSelection {
    pub service: RunOpenCodeService,
    pub model_id: String,
    pub protocol_revision: u16,
    pub maximum_input_tokens: u32,
    pub maximum_output_tokens: u32,
    pub supports_tool_calls: bool,
    pub supports_image_input: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ContextCheckpointId([u8; IDENTIFIER_BYTES]);

impl ContextCheckpointId {
    pub(crate) const fn from_bytes(bytes: [u8; IDENTIFIER_BYTES]) -> Self {
        Self(bytes)
    }

    pub(crate) const fn as_bytes(&self) -> &[u8; IDENTIFIER_BYTES] {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ContextCheckpoint {
    pub id: ContextCheckpointId,
    pub source_entry_high_water: u64,
    pub source_digest: [u8; 32],
    pub summary: String,
    pub estimated_summary_tokens: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct CompactionPlan {
    pub parent_checkpoint_id: Option<ContextCheckpointId>,
    pub user_guidance: Option<String>,
    pub source_entry_high_water: u64,
    pub source_digest: [u8; 32],
    pub entries: Vec<TranscriptEntry>,
    pub parent_summary: Option<String>,
    pub estimated_input_tokens: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CompactionOperationId([u8; IDENTIFIER_BYTES]);

impl CompactionOperationId {
    pub(crate) const fn from_bytes(bytes: [u8; IDENTIFIER_BYTES]) -> Self {
        Self(bytes)
    }

    pub(crate) const fn as_bytes(&self) -> &[u8; IDENTIFIER_BYTES] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SessionContextStatus {
    pub estimated_input_tokens: u32,
    pub maximum_input_tokens: u32,
    pub maximum_output_tokens: u32,
    pub compaction_threshold_tokens: u32,
    pub checkpoint_source_entry_high_water: Option<u64>,
    pub checkpoint_estimated_summary_tokens: Option<u32>,
}

#[derive(Clone, Debug)]
pub(crate) struct RunContext {
    pub run: Run,
    pub skills: crate::skills::RunSkillContext,
    pub attachment_data: std::collections::HashMap<ImageAttachmentId, Vec<u8>>,
    pub checkpoint: Option<ContextCheckpoint>,
    pub compaction_plan: Option<CompactionPlan>,
    pub entries: Vec<TranscriptEntry>,
    pub current_entry_high_water: u64,
    pub estimated_input_tokens: u32,
    pub working_directory: Option<String>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ToolOperationId([u8; IDENTIFIER_BYTES]);

impl ToolOperationId {
    pub(crate) const fn from_bytes(bytes: [u8; IDENTIFIER_BYTES]) -> Self {
        Self(bytes)
    }

    pub(crate) const fn as_bytes(&self) -> &[u8; IDENTIFIER_BYTES] {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CommittedToolCall {
    pub call_id: ToolCallId,
    pub operation_id: ToolOperationId,
    pub input: ToolInput,
}

#[derive(Clone, Debug)]
pub(crate) struct CompletedToolTurn {
    pub provider_response_id: String,
    pub usage: ProviderUsage,
    pub commentary: Option<(String, bool)>,
    pub calls: Vec<ValidatedProviderCall>,
}

#[derive(Clone, Debug)]
pub(crate) struct CommittedToolTurn {
    pub calls: Vec<CommittedToolCall>,
}

#[derive(Clone, Debug)]
pub(crate) struct ToolOperationRecovery {
    pub run_id: RunId,
    pub call_id: ToolCallId,
    pub operation_id: ToolOperationId,
    pub input: ToolInput,
    pub prepared: bool,
    pub dispatched: bool,
    pub recovery_plan: Option<Vec<u8>>,
}

fn write_hex(formatter: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    for byte in bytes {
        write!(formatter, "{byte:02x}")?;
    }
    Ok(())
}
