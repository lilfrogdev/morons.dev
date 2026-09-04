use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::{APPLICATION_IDENTIFIER_BYTES, SessionId};

const RUN_ID_PREFIX: &str = "run_";
const MESSAGE_ID_PREFIX: &str = "msg_";
const TOOL_CALL_ID_PREFIX: &str = "tool_";
const IMAGE_ATTACHMENT_ID_PREFIX: &str = "img_";
const LOCAL_COMMAND_ID_PREFIX: &str = "cmd_";
const TRANSCRIPT_CURSOR_PREFIX: &str = "tc2_";
const TRANSCRIPT_CURSOR_BYTES: usize = 40;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct RunId([u8; APPLICATION_IDENTIFIER_BYTES]);

impl RunId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; APPLICATION_IDENTIFIER_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; APPLICATION_IDENTIFIER_BYTES] {
        &self.0
    }
}

impl fmt::Debug for RunId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_prefixed_hex(formatter, RUN_ID_PREFIX, &self.0)
    }
}

impl Serialize for RunId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&encode_prefixed_hex(RUN_ID_PREFIX, &self.0))
    }
}

impl<'de> Deserialize<'de> for RunId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        decode_prefixed_hex(&encoded, RUN_ID_PREFIX)
            .map(Self)
            .map_err(de::Error::custom)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct MessageId([u8; APPLICATION_IDENTIFIER_BYTES]);

impl MessageId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; APPLICATION_IDENTIFIER_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; APPLICATION_IDENTIFIER_BYTES] {
        &self.0
    }
}

impl fmt::Debug for MessageId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_prefixed_hex(formatter, MESSAGE_ID_PREFIX, &self.0)
    }
}

impl Serialize for MessageId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&encode_prefixed_hex(MESSAGE_ID_PREFIX, &self.0))
    }
}

impl<'de> Deserialize<'de> for MessageId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        decode_prefixed_hex(&encoded, MESSAGE_ID_PREFIX)
            .map(Self)
            .map_err(de::Error::custom)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageAttachmentId([u8; APPLICATION_IDENTIFIER_BYTES]);

impl ImageAttachmentId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; APPLICATION_IDENTIFIER_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; APPLICATION_IDENTIFIER_BYTES] {
        &self.0
    }
}

impl fmt::Debug for ImageAttachmentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_prefixed_hex(formatter, IMAGE_ATTACHMENT_ID_PREFIX, &self.0)
    }
}

impl Serialize for ImageAttachmentId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&encode_prefixed_hex(IMAGE_ATTACHMENT_ID_PREFIX, &self.0))
    }
}

impl<'de> Deserialize<'de> for ImageAttachmentId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        decode_prefixed_hex(&encoded, IMAGE_ATTACHMENT_ID_PREFIX)
            .map(Self)
            .map_err(de::Error::custom)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ToolCallId([u8; APPLICATION_IDENTIFIER_BYTES]);

impl ToolCallId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; APPLICATION_IDENTIFIER_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; APPLICATION_IDENTIFIER_BYTES] {
        &self.0
    }
}

impl fmt::Debug for ToolCallId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_prefixed_hex(formatter, TOOL_CALL_ID_PREFIX, &self.0)
    }
}

impl Serialize for ToolCallId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&encode_prefixed_hex(TOOL_CALL_ID_PREFIX, &self.0))
    }
}

impl<'de> Deserialize<'de> for ToolCallId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        decode_prefixed_hex(&encoded, TOOL_CALL_ID_PREFIX)
            .map(Self)
            .map_err(de::Error::custom)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalCommandId([u8; APPLICATION_IDENTIFIER_BYTES]);

impl LocalCommandId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; APPLICATION_IDENTIFIER_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; APPLICATION_IDENTIFIER_BYTES] {
        &self.0
    }
}

impl fmt::Debug for LocalCommandId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_prefixed_hex(formatter, LOCAL_COMMAND_ID_PREFIX, &self.0)
    }
}

impl Serialize for LocalCommandId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&encode_prefixed_hex(LOCAL_COMMAND_ID_PREFIX, &self.0))
    }
}

impl<'de> Deserialize<'de> for LocalCommandId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        decode_prefixed_hex(&encoded, LOCAL_COMMAND_ID_PREFIX)
            .map(Self)
            .map_err(de::Error::custom)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TranscriptCursor([u8; TRANSCRIPT_CURSOR_BYTES]);

impl TranscriptCursor {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; TRANSCRIPT_CURSOR_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; TRANSCRIPT_CURSOR_BYTES] {
        &self.0
    }
}

impl fmt::Debug for TranscriptCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_prefixed_hex(formatter, TRANSCRIPT_CURSOR_PREFIX, &self.0)
    }
}

impl Serialize for TranscriptCursor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&encode_prefixed_hex(TRANSCRIPT_CURSOR_PREFIX, &self.0))
    }
}

impl<'de> Deserialize<'de> for TranscriptCursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        decode_prefixed_hex(&encoded, TRANSCRIPT_CURSOR_PREFIX)
            .map(Self)
            .map_err(de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptPageDirection {
    Older,
    Newer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    Responses,
    ChatCompletions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenCodeService {
    Zen,
    Go,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenCodeModelTrainingUse {
    NotUsed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenCodeModelRetention {
    None,
    UpToThirtyDays,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeModelCapabilities {
    pub text_input: bool,
    pub image_input: bool,
    pub text_output: bool,
    pub reasoning: bool,
    pub reasoning_continuation: bool,
    pub tool_calls: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeModelSelection {
    pub service: OpenCodeService,
    pub model_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeModelSummary {
    pub service: OpenCodeService,
    pub id: String,
    pub display_name: String,
    pub available: bool,
    pub protocol: ProviderProtocol,
    pub protocol_revision: u16,
    pub capabilities: OpenCodeModelCapabilities,
    pub maximum_input_tokens: u32,
    pub maximum_output_tokens: u32,
    pub training_use: OpenCodeModelTrainingUse,
    pub retention: OpenCodeModelRetention,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunSummary {
    pub id: RunId,
    pub session_id: SessionId,
    pub user_message_id: MessageId,
    pub service: OpenCodeService,
    pub model_id: String,
    pub protocol_revision: u16,
    pub credential_generation: u64,
    pub context_policy_version: u16,
    pub tool_catalog_version: u16,
    pub tool_limits_version: u16,
    pub state: RunState,
    pub cancellation_requested: bool,
    pub failure: Option<RunFailureKind>,
    pub accepted_at_milliseconds: u64,
    pub updated_at_milliseconds: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    ListDirectory,
    ReadFile,
    SearchText,
    EditFile,
    CreateFile,
    CreateDirectory,
    // Retained for historical transcript compatibility; it is not an offered MVP tool.
    RunCommand,
    Read,
    Write,
    Edit,
    Bash,
    WebSearch,
    Ipython,
    Task,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalCommandStatus {
    Succeeded,
    Failed,
    Interrupted,
    Uncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultStatus {
    Succeeded,
    Failed,
    Interrupted,
    Uncertain,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageUpload {
    pub display_name: String,
    pub marker_start: u32,
    pub data_base64: String,
}

impl fmt::Debug for ImageUpload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImageUpload")
            .field("display_name_bytes", &self.display_name.len())
            .field("marker_start", &self.marker_start)
            .field("data_base64", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageAttachmentSummary {
    pub id: ImageAttachmentId,
    pub display_name: String,
    pub media_type: String,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    pub marker_start: u32,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "entry", rename_all = "snake_case", deny_unknown_fields)]
pub enum TranscriptEntry {
    UserMessage {
        id: MessageId,
        run_id: RunId,
        text: String,
        attachments: Vec<ImageAttachmentSummary>,
        created_at_milliseconds: u64,
    },
    AssistantMessage {
        id: MessageId,
        run_id: RunId,
        service: OpenCodeService,
        model_id: String,
        text: String,
        refusal: bool,
        created_at_milliseconds: u64,
    },
    ToolCall {
        id: MessageId,
        run_id: RunId,
        call_id: ToolCallId,
        tool: ToolKind,
        path: String,
        created_at_milliseconds: u64,
    },
    ToolResult {
        id: MessageId,
        run_id: RunId,
        call_id: ToolCallId,
        tool: ToolKind,
        status: ToolResultStatus,
        summary: String,
        created_at_milliseconds: u64,
    },
    LocalCommand {
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

impl fmt::Debug for TranscriptEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UserMessage {
                id,
                run_id,
                text,
                attachments,
                created_at_milliseconds,
            } => formatter
                .debug_struct("UserMessage")
                .field("id", id)
                .field("run_id", run_id)
                .field("text_bytes", &text.len())
                .field("attachments", &attachments.len())
                .field("created_at_milliseconds", created_at_milliseconds)
                .finish(),
            Self::AssistantMessage {
                id,
                run_id,
                service,
                model_id,
                text,
                refusal,
                created_at_milliseconds,
            } => formatter
                .debug_struct("AssistantMessage")
                .field("id", id)
                .field("run_id", run_id)
                .field("service", service)
                .field("model_id", model_id)
                .field("text_bytes", &text.len())
                .field("refusal", refusal)
                .field("created_at_milliseconds", created_at_milliseconds)
                .finish(),
            Self::ToolCall {
                id,
                run_id,
                call_id,
                tool,
                path,
                created_at_milliseconds,
            } => formatter
                .debug_struct("ToolCall")
                .field("id", id)
                .field("run_id", run_id)
                .field("call_id", call_id)
                .field("tool", tool)
                .field("path_bytes", &path.len())
                .field("created_at_milliseconds", created_at_milliseconds)
                .finish(),
            Self::ToolResult {
                id,
                run_id,
                call_id,
                tool,
                status,
                summary,
                created_at_milliseconds,
            } => formatter
                .debug_struct("ToolResult")
                .field("id", id)
                .field("run_id", run_id)
                .field("call_id", call_id)
                .field("tool", tool)
                .field("status", status)
                .field("summary_bytes", &summary.len())
                .field("created_at_milliseconds", created_at_milliseconds)
                .finish(),
            Self::LocalCommand {
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
mod tests {
    use super::*;

    #[test]
    fn run_and_message_identifiers_round_trip() {
        let run = RunId::from_bytes([0x11; APPLICATION_IDENTIFIER_BYTES]);
        let message = MessageId::from_bytes([0x22; APPLICATION_IDENTIFIER_BYTES]);
        let tool = ToolCallId::from_bytes([0x23; APPLICATION_IDENTIFIER_BYTES]);

        let run_json = serde_json::to_vec(&run).expect("run identifier should encode");
        let message_json = serde_json::to_vec(&message).expect("message identifier should encode");
        let tool_json = serde_json::to_vec(&tool).expect("tool identifier should encode");

        assert_eq!(
            serde_json::from_slice::<RunId>(&run_json).expect("run identifier should decode"),
            run
        );
        assert_eq!(
            serde_json::from_slice::<MessageId>(&message_json)
                .expect("message identifier should decode"),
            message
        );
        assert_eq!(
            serde_json::from_slice::<ToolCallId>(&tool_json)
                .expect("tool identifier should decode"),
            tool
        );
        assert!(format!("{run:?}").starts_with(RUN_ID_PREFIX));
        assert!(format!("{message:?}").starts_with(MESSAGE_ID_PREFIX));
        assert!(format!("{tool:?}").starts_with(TOOL_CALL_ID_PREFIX));
    }

    #[test]
    fn transcript_cursor_is_strict_and_opaque() {
        let cursor = TranscriptCursor::from_bytes([0x33; TRANSCRIPT_CURSOR_BYTES]);
        let encoded = serde_json::to_vec(&cursor).expect("cursor should encode");
        assert_eq!(
            serde_json::from_slice::<TranscriptCursor>(&encoded).expect("cursor should decode"),
            cursor
        );
        assert!(
            std::str::from_utf8(&encoded)
                .expect("cursor JSON should be UTF-8")
                .starts_with("\"tc2_")
        );
        assert!(serde_json::from_str::<TranscriptCursor>("\"tc1_AA\"").is_err());
    }

    #[test]
    fn transcript_debug_omits_message_text() {
        let entry = TranscriptEntry::UserMessage {
            id: MessageId::from_bytes([0x44; APPLICATION_IDENTIFIER_BYTES]),
            run_id: RunId::from_bytes([0x55; APPLICATION_IDENTIFIER_BYTES]),
            text: "sensitive transcript text".to_owned(),
            attachments: Vec::new(),
            created_at_milliseconds: 1,
        };
        let debug = format!("{entry:?}");
        assert!(!debug.contains("sensitive transcript text"));
        assert!(debug.contains("text_bytes"));
    }

    #[test]
    fn image_upload_debug_redacts_payload_and_identifier_is_strict() {
        let upload = ImageUpload {
            display_name: "picture.png".to_owned(),
            marker_start: 4,
            data_base64: "c2Vuc2l0aXZlIGltYWdl".to_owned(),
        };
        let debug = format!("{upload:?}");
        assert!(!debug.contains("c2Vuc2l0aXZlIGltYWdl"));
        assert!(debug.contains("[REDACTED]"));
        let id = ImageAttachmentId::from_bytes([0x45; APPLICATION_IDENTIFIER_BYTES]);
        assert!(format!("{id:?}").starts_with("img_"));
        let encoded = serde_json::to_string(&id).expect("identifier should encode");
        assert_eq!(
            serde_json::from_str::<ImageAttachmentId>(&encoded).expect("identifier should decode"),
            id
        );
    }

    #[test]
    fn tool_transcript_debug_omits_paths_and_results() {
        let call = TranscriptEntry::ToolCall {
            id: MessageId::from_bytes([0x61; APPLICATION_IDENTIFIER_BYTES]),
            run_id: RunId::from_bytes([0x62; APPLICATION_IDENTIFIER_BYTES]),
            call_id: ToolCallId::from_bytes([0x63; APPLICATION_IDENTIFIER_BYTES]),
            tool: ToolKind::ReadFile,
            path: "sensitive/repository/path".to_owned(),
            created_at_milliseconds: 1,
        };
        let result = TranscriptEntry::ToolResult {
            id: MessageId::from_bytes([0x64; APPLICATION_IDENTIFIER_BYTES]),
            run_id: RunId::from_bytes([0x62; APPLICATION_IDENTIFIER_BYTES]),
            call_id: ToolCallId::from_bytes([0x63; APPLICATION_IDENTIFIER_BYTES]),
            tool: ToolKind::ReadFile,
            status: ToolResultStatus::Succeeded,
            summary: "sensitive repository result".to_owned(),
            created_at_milliseconds: 2,
        };
        assert!(!format!("{call:?}").contains("sensitive/repository/path"));
        assert!(!format!("{result:?}").contains("sensitive repository result"));
    }

    #[test]
    fn task_tool_kind_has_a_stable_wire_name() {
        assert_eq!(
            serde_json::to_string(&ToolKind::Task).expect("tool kind should encode"),
            "\"task\""
        );
        assert_eq!(
            serde_json::from_str::<ToolKind>("\"task\"").expect("tool kind should decode"),
            ToolKind::Task
        );
    }

    #[test]
    fn only_terminal_run_states_are_terminal() {
        assert!(!RunState::Accepted.is_terminal());
        assert!(!RunState::Active.is_terminal());
        for state in [
            RunState::Succeeded,
            RunState::Failed,
            RunState::Cancelled,
            RunState::Interrupted,
            RunState::Uncertain,
        ] {
            assert!(state.is_terminal());
        }
    }
}
