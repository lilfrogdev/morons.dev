use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

pub const APPLICATION_IDENTIFIER_BYTES: usize = 16;
pub const MAX_REPOSITORY_SOURCE_PATH_BYTES: usize = 4096;
const SESSION_LIST_CURSOR_BYTES: usize = 16;
const SESSION_CATALOG_CURSOR_BYTES: usize = 8;
const SESSION_EVENT_CURSOR_BYTES: usize = 24;
const SESSION_ID_PREFIX: &str = "ses_";
const MUTATION_REQUEST_ID_PREFIX: &str = "mut_";
const SESSION_LIST_CURSOR_PREFIX: &str = "sc2_";
const SESSION_CATALOG_CURSOR_PREFIX: &str = "scc1_";
const SESSION_EVENT_CURSOR_PREFIX: &str = "sec1_";

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId([u8; APPLICATION_IDENTIFIER_BYTES]);

impl SessionId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; APPLICATION_IDENTIFIER_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; APPLICATION_IDENTIFIER_BYTES] {
        &self.0
    }
}

impl fmt::Debug for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_prefixed_hex(formatter, SESSION_ID_PREFIX, &self.0)
    }
}

impl Serialize for SessionId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&encode_prefixed_hex(SESSION_ID_PREFIX, &self.0))
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        decode_prefixed_hex(&encoded, SESSION_ID_PREFIX)
            .map(Self)
            .map_err(de::Error::custom)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct MutationRequestId([u8; APPLICATION_IDENTIFIER_BYTES]);

impl MutationRequestId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; APPLICATION_IDENTIFIER_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; APPLICATION_IDENTIFIER_BYTES] {
        &self.0
    }
}

impl fmt::Debug for MutationRequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_prefixed_hex(formatter, MUTATION_REQUEST_ID_PREFIX, &self.0)
    }
}

impl Serialize for MutationRequestId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&encode_prefixed_hex(MUTATION_REQUEST_ID_PREFIX, &self.0))
    }
}

impl<'de> Deserialize<'de> for MutationRequestId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        decode_prefixed_hex(&encoded, MUTATION_REQUEST_ID_PREFIX)
            .map(Self)
            .map_err(de::Error::custom)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionListCursor([u8; SESSION_LIST_CURSOR_BYTES]);

impl SessionListCursor {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; SESSION_LIST_CURSOR_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; SESSION_LIST_CURSOR_BYTES] {
        &self.0
    }
}

impl fmt::Debug for SessionListCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_prefixed_hex(formatter, SESSION_LIST_CURSOR_PREFIX, &self.0)
    }
}

impl Serialize for SessionListCursor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&encode_prefixed_hex(SESSION_LIST_CURSOR_PREFIX, &self.0))
    }
}

impl<'de> Deserialize<'de> for SessionListCursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        decode_prefixed_hex(&encoded, SESSION_LIST_CURSOR_PREFIX)
            .map(Self)
            .map_err(de::Error::custom)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionCatalogEventCursor([u8; SESSION_CATALOG_CURSOR_BYTES]);

impl SessionCatalogEventCursor {
    #[must_use]
    pub const fn beginning() -> Self {
        Self([0; SESSION_CATALOG_CURSOR_BYTES])
    }

    #[must_use]
    pub const fn from_bytes(bytes: [u8; SESSION_CATALOG_CURSOR_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; SESSION_CATALOG_CURSOR_BYTES] {
        &self.0
    }
}

impl fmt::Debug for SessionCatalogEventCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_prefixed_hex(formatter, SESSION_CATALOG_CURSOR_PREFIX, &self.0)
    }
}

impl Serialize for SessionCatalogEventCursor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&encode_prefixed_hex(SESSION_CATALOG_CURSOR_PREFIX, &self.0))
    }
}

impl<'de> Deserialize<'de> for SessionCatalogEventCursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        decode_prefixed_hex(&encoded, SESSION_CATALOG_CURSOR_PREFIX)
            .map(Self)
            .map_err(de::Error::custom)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionEventCursor([u8; SESSION_EVENT_CURSOR_BYTES]);

impl SessionEventCursor {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; SESSION_EVENT_CURSOR_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; SESSION_EVENT_CURSOR_BYTES] {
        &self.0
    }
}

impl fmt::Debug for SessionEventCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_prefixed_hex(formatter, SESSION_EVENT_CURSOR_PREFIX, &self.0)
    }
}

impl Serialize for SessionEventCursor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&encode_prefixed_hex(SESSION_EVENT_CURSOR_PREFIX, &self.0))
    }
}

impl<'de> Deserialize<'de> for SessionEventCursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        decode_prefixed_hex(&encoded, SESSION_EVENT_CURSOR_PREFIX)
            .map(Self)
            .map_err(de::Error::custom)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApplicationRequest {
    CreateSession {
        mutation_request_id: MutationRequestId,
        display_name: Option<String>,
    },
    GetSession {
        session_id: SessionId,
    },
    ListSessions {
        cursor: Option<SessionListCursor>,
        limit: u16,
    },
    SubscribeSessionCatalog {
        cursor: SessionCatalogEventCursor,
    },
    ListOpenCodeModels {
        service: crate::OpenCodeService,
    },
    GetOpenCodeCredentialStatus,
    GetExecutionImageStatus,
    ProvisionExecutionImage {
        mutation_request_id: MutationRequestId,
        toolchain_source_path: String,
        cargo_source_path: String,
    },
    SetOpenCodeCredential {
        mutation_request_id: MutationRequestId,
        expected_generation: u64,
        api_key: crate::OpenCodeApiKey,
    },
    RemoveOpenCodeCredential {
        mutation_request_id: MutationRequestId,
        expected_generation: u64,
    },
    ImportRepository {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        source_path: String,
    },
    SubmitSessionInput {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        text: String,
        service: crate::OpenCodeService,
        model_id: String,
    },
    GetRun {
        session_id: SessionId,
        run_id: crate::RunId,
    },
    ListSessionTranscript {
        session_id: SessionId,
        cursor: Option<crate::TranscriptCursor>,
        limit: u16,
    },
    SubscribeSession {
        session_id: SessionId,
        cursor: SessionEventCursor,
    },
    CancelRun {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        run_id: crate::RunId,
    },
    AcknowledgeToolUncertainty {
        mutation_request_id: MutationRequestId,
        session_id: SessionId,
        run_id: crate::RunId,
    },
    StopServer {
        mutation_request_id: MutationRequestId,
    },
}

impl fmt::Debug for ApplicationRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateSession {
                mutation_request_id,
                display_name,
            } => formatter
                .debug_struct("CreateSession")
                .field("mutation_request_id", mutation_request_id)
                .field("display_name", display_name)
                .finish(),
            Self::GetSession { session_id } => formatter
                .debug_struct("GetSession")
                .field("session_id", session_id)
                .finish(),
            Self::ListSessions { cursor, limit } => formatter
                .debug_struct("ListSessions")
                .field("cursor", cursor)
                .field("limit", limit)
                .finish(),
            Self::SubscribeSessionCatalog { cursor } => formatter
                .debug_struct("SubscribeSessionCatalog")
                .field("cursor", cursor)
                .finish(),
            Self::ListOpenCodeModels { service } => formatter
                .debug_struct("ListOpenCodeModels")
                .field("service", service)
                .finish(),
            Self::GetOpenCodeCredentialStatus => formatter.write_str("GetOpenCodeCredentialStatus"),
            Self::GetExecutionImageStatus => formatter.write_str("GetExecutionImageStatus"),
            Self::ProvisionExecutionImage {
                mutation_request_id,
                toolchain_source_path,
                cargo_source_path,
            } => formatter
                .debug_struct("ProvisionExecutionImage")
                .field("mutation_request_id", mutation_request_id)
                .field("toolchain_source_path_bytes", &toolchain_source_path.len())
                .field("cargo_source_path_bytes", &cargo_source_path.len())
                .finish(),
            Self::SetOpenCodeCredential {
                mutation_request_id,
                expected_generation,
                api_key,
            } => formatter
                .debug_struct("SetOpenCodeCredential")
                .field("mutation_request_id", mutation_request_id)
                .field("expected_generation", expected_generation)
                .field("api_key", api_key)
                .finish(),
            Self::RemoveOpenCodeCredential {
                mutation_request_id,
                expected_generation,
            } => formatter
                .debug_struct("RemoveOpenCodeCredential")
                .field("mutation_request_id", mutation_request_id)
                .field("expected_generation", expected_generation)
                .finish(),
            Self::ImportRepository {
                mutation_request_id,
                session_id,
                source_path,
            } => formatter
                .debug_struct("ImportRepository")
                .field("mutation_request_id", mutation_request_id)
                .field("session_id", session_id)
                .field("source_path_bytes", &source_path.len())
                .finish(),
            Self::SubmitSessionInput {
                mutation_request_id,
                session_id,
                text,
                service,
                model_id,
            } => formatter
                .debug_struct("SubmitSessionInput")
                .field("mutation_request_id", mutation_request_id)
                .field("session_id", session_id)
                .field("text_bytes", &text.len())
                .field("service", service)
                .field("model_id", model_id)
                .finish(),
            Self::GetRun { session_id, run_id } => formatter
                .debug_struct("GetRun")
                .field("session_id", session_id)
                .field("run_id", run_id)
                .finish(),
            Self::ListSessionTranscript {
                session_id,
                cursor,
                limit,
            } => formatter
                .debug_struct("ListSessionTranscript")
                .field("session_id", session_id)
                .field("cursor", cursor)
                .field("limit", limit)
                .finish(),
            Self::SubscribeSession { session_id, cursor } => formatter
                .debug_struct("SubscribeSession")
                .field("session_id", session_id)
                .field("cursor", cursor)
                .finish(),
            Self::CancelRun {
                mutation_request_id,
                session_id,
                run_id,
            } => formatter
                .debug_struct("CancelRun")
                .field("mutation_request_id", mutation_request_id)
                .field("session_id", session_id)
                .field("run_id", run_id)
                .finish(),
            Self::AcknowledgeToolUncertainty {
                mutation_request_id,
                session_id,
                run_id,
            } => formatter
                .debug_struct("AcknowledgeToolUncertainty")
                .field("mutation_request_id", mutation_request_id)
                .field("session_id", session_id)
                .field("run_id", run_id)
                .finish(),
            Self::StopServer {
                mutation_request_id,
            } => formatter
                .debug_struct("StopServer")
                .field("mutation_request_id", mutation_request_id)
                .finish(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApplicationResponse {
    SessionCreated {
        session: SessionSummary,
    },
    SessionFound {
        session: SessionSummary,
    },
    SessionsListed {
        sessions: Vec<SessionSummary>,
        next_cursor: Option<SessionListCursor>,
        catalog_cursor: SessionCatalogEventCursor,
    },
    SessionCatalogSubscriptionStarted {
        cursor: SessionCatalogEventCursor,
    },
    OpenCodeModelsListed {
        service: crate::OpenCodeService,
        models: Vec<crate::OpenCodeModelSummary>,
    },
    OpenCodeCredentialStatus {
        credential: crate::OpenCodeCredentialStatus,
    },
    OpenCodeCredentialUpdated {
        credential: crate::OpenCodeCredentialStatus,
    },
    ExecutionImageStatus {
        image: crate::ExecutionImageSummary,
    },
    ExecutionImageProvisioned {
        image: crate::ExecutionImageSummary,
    },
    RepositoryImported {
        session_id: SessionId,
        workspace: WorkspaceSummary,
    },
    SessionInputAccepted {
        user_message_id: crate::MessageId,
        run: crate::RunSummary,
    },
    RunFound {
        run: crate::RunSummary,
    },
    SessionTranscriptListed {
        session: SessionSummary,
        workspace: WorkspaceSummary,
        entries: Vec<crate::TranscriptEntry>,
        runs: Vec<crate::RunSummary>,
        active_run_id: Option<crate::RunId>,
        next_cursor: Option<crate::TranscriptCursor>,
        event_cursor: SessionEventCursor,
    },
    SessionSubscriptionStarted {
        session_id: SessionId,
        cursor: SessionEventCursor,
    },
    RunCancellationResolved {
        run_id: crate::RunId,
        state: crate::RunState,
        cancellation_requested: bool,
    },
    ToolUncertaintyAcknowledged {
        session_id: SessionId,
        run_id: crate::RunId,
        workspace: WorkspaceSummary,
    },
    ServerStopAccepted {
        current_server_stopping: bool,
    },
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApplicationEvent {
    SessionCreated {
        cursor: SessionCatalogEventCursor,
        session: SessionSummary,
    },
    SessionTranscriptEntryCommitted {
        cursor: SessionEventCursor,
        session_id: SessionId,
        entry: crate::TranscriptEntry,
    },
    SessionRunChanged {
        cursor: SessionEventCursor,
        run: crate::RunSummary,
    },
    SessionWorkspaceChanged {
        cursor: SessionEventCursor,
        session_id: SessionId,
        workspace: WorkspaceSummary,
    },
    SessionAssistantDelta {
        session_id: SessionId,
        run_id: crate::RunId,
        sequence: u64,
        delta: String,
        refusal: bool,
    },
}

impl ApplicationEvent {
    #[must_use]
    pub const fn session_catalog_cursor(&self) -> Option<SessionCatalogEventCursor> {
        match self {
            Self::SessionCreated { cursor, .. } => Some(*cursor),
            Self::SessionTranscriptEntryCommitted { .. }
            | Self::SessionRunChanged { .. }
            | Self::SessionWorkspaceChanged { .. }
            | Self::SessionAssistantDelta { .. } => None,
        }
    }

    #[must_use]
    pub const fn session_cursor(&self) -> Option<SessionEventCursor> {
        match self {
            Self::SessionTranscriptEntryCommitted { cursor, .. }
            | Self::SessionRunChanged { cursor, .. }
            | Self::SessionWorkspaceChanged { cursor, .. } => Some(*cursor),
            Self::SessionCreated { .. } | Self::SessionAssistantDelta { .. } => None,
        }
    }
}

impl fmt::Debug for ApplicationEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SessionCreated { cursor, session } => formatter
                .debug_struct("SessionCreated")
                .field("cursor", cursor)
                .field("session", session)
                .finish(),
            Self::SessionTranscriptEntryCommitted {
                cursor,
                session_id,
                entry,
            } => formatter
                .debug_struct("SessionTranscriptEntryCommitted")
                .field("cursor", cursor)
                .field("session_id", session_id)
                .field("entry", entry)
                .finish(),
            Self::SessionRunChanged { cursor, run } => formatter
                .debug_struct("SessionRunChanged")
                .field("cursor", cursor)
                .field("run", run)
                .finish(),
            Self::SessionWorkspaceChanged {
                cursor,
                session_id,
                workspace,
            } => formatter
                .debug_struct("SessionWorkspaceChanged")
                .field("cursor", cursor)
                .field("session_id", session_id)
                .field("workspace", workspace)
                .finish(),
            Self::SessionAssistantDelta {
                session_id,
                run_id,
                sequence,
                delta,
                refusal,
            } => formatter
                .debug_struct("SessionAssistantDelta")
                .field("session_id", session_id)
                .field("run_id", run_id)
                .field("sequence", sequence)
                .field("delta_bytes", &delta.len())
                .field("refusal", refusal)
                .finish(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSummary {
    pub id: SessionId,
    pub display_name: Option<String>,
    pub created_at_milliseconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceState {
    Empty,
    Importing,
    Ready,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceBlockReason {
    InconsistentImportState,
    UncertainToolEffect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSummary {
    pub state: WorkspaceState,
    pub file_count: u64,
    pub logical_bytes: u64,
    pub block_reason: Option<WorkspaceBlockReason>,
    pub blocked_run_id: Option<crate::RunId>,
    pub blocked_tool: Option<crate::ToolKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApplicationError {
    InvalidRequest,
    RequestConflict,
    SessionNotFound,
    RunNotFound,
    SessionBusy { active_run_id: crate::RunId },
    UnsupportedModel,
    OpenCodeCredentialNotConfigured,
    CredentialGenerationConflict,
    CredentialMutationNotApplied,
    ExecutionImageProvisionNotApplied,
    ExecutionImageBlocked,
    WorkspaceNotPristine,
    WorkspaceBusy,
    RepositoryAlreadyImported,
    RepositoryImportNotApplied,
    WorkspaceBlocked,
    ResourceLimit { resource: ResourceLimit },
    ServiceUnavailable,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceLimit {
    Sessions,
    Runs,
    Context,
    Storage,
}

fn encode_prefixed_hex(prefix: &str, bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(prefix.len() + bytes.len() * 2);
    encoded.push_str(prefix);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn decode_prefixed_hex<const N: usize>(
    encoded: &str,
    prefix: &str,
) -> Result<[u8; N], &'static str> {
    let Some(hex) = encoded.strip_prefix(prefix) else {
        return Err("an opaque identifier has an unexpected prefix");
    };
    if hex.len() != N * 2 {
        return Err("an opaque identifier has an unexpected length");
    }

    let mut decoded = [0_u8; N];
    let (pairs, _) = hex.as_bytes().as_chunks::<2>();
    for (index, pair) in pairs.iter().enumerate() {
        let high = decode_hex_digit(pair[0])?;
        let low = decode_hex_digit(pair[1])?;
        decoded[index] = high << 4 | low;
    }
    Ok(decoded)
}

fn decode_hex_digit(byte: u8) -> Result<u8, &'static str> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err("an opaque identifier must use lowercase hexadecimal digits"),
    }
}

fn write_prefixed_hex(
    formatter: &mut fmt::Formatter<'_>,
    prefix: &str,
    bytes: &[u8],
) -> fmt::Result {
    formatter.write_str(prefix)?;
    for byte in bytes {
        write!(formatter, "{byte:02x}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
