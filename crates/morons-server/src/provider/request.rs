use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::OnceLock,
};

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::{
    OpenCodeModel, OpenCodeService, ProviderError, ProviderProtocol, find_open_code_model,
    json::parse_strict_value,
};

pub const MAX_PROVIDER_REQUEST_BYTES: usize = 16 * 1024 * 1024;
const MAX_INPUT_ITEMS: usize = 256;
const MAX_INPUT_TEXT_BYTES: usize = 512 * 1024;
const MAX_AGGREGATE_INPUT_BYTES: usize = 12 * 1024 * 1024;
const MAX_INPUT_IMAGES: usize = 16;
const MAX_INPUT_IMAGE_BYTES: usize = 2 * 1024 * 1024;
const MAX_AGGREGATE_IMAGE_BYTES: usize = 6 * 1024 * 1024;
pub(super) const MAX_TOOL_COUNT: usize = 64;
pub(super) const MAX_TOOL_NAME_BYTES: usize = 64;
const MAX_TOOL_DESCRIPTION_BYTES: usize = 4 * 1024;
const MAX_TOOL_SCHEMA_BYTES: usize = 256 * 1024;
const MAX_TOOL_SCHEMA_DEPTH: usize = 32;
const MAX_TOOL_SCHEMA_NODES: usize = 4_096;
pub(super) const MAX_PROVIDER_CALL_ID_BYTES: usize = 128;
pub(super) const MAX_TOOL_ARGUMENT_BYTES: usize = 256 * 1024;
const MAX_TOOL_RESULT_BYTES: usize = 512 * 1024;
const MAX_REASONING_SUMMARIES: usize = 64;
const MAX_REASONING_SUMMARY_BYTES: usize = 256 * 1024;
const MAX_ENCRYPTED_REASONING_BYTES: usize = 512 * 1024;
const OPENCODE_SESSION_FINGERPRINT_CONTEXT: &[u8] = b"morons.dev/opencode-session/v1\0";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderMessageRole {
    Developer,
    User,
    Assistant,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderMessagePhase {
    Commentary,
    FinalAnswer,
}

#[derive(Clone, PartialEq, Eq)]
pub enum ProviderContentPart {
    Text(String),
    Image {
        media_type: morons_image::ImageMediaType,
        width: u32,
        height: u32,
        bytes: Vec<u8>,
    },
}

impl fmt::Debug for ProviderContentPart {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(text) => formatter
                .debug_struct("Text")
                .field("text_bytes", &text.len())
                .finish(),
            Self::Image {
                media_type,
                width,
                height,
                bytes,
            } => formatter
                .debug_struct("Image")
                .field("media_type", media_type)
                .field("width", width)
                .field("height", height)
                .field("bytes", &bytes.len())
                .finish(),
        }
    }
}

#[derive(Clone)]
pub enum ProviderInputItem {
    Message {
        role: ProviderMessageRole,
        text: String,
        phase: Option<ProviderMessagePhase>,
    },
    MultimodalMessage {
        role: ProviderMessageRole,
        parts: Vec<ProviderContentPart>,
        phase: Option<ProviderMessagePhase>,
    },
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
        opaque_continuation: Option<String>,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
    Reasoning {
        id: String,
        summaries: Vec<String>,
        encrypted_content: Option<String>,
    },
}

impl fmt::Debug for ProviderInputItem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message { role, text, phase } => formatter
                .debug_struct("Message")
                .field("role", role)
                .field("phase", phase)
                .field("text_bytes", &text.len())
                .finish(),
            Self::MultimodalMessage { role, parts, phase } => formatter
                .debug_struct("MultimodalMessage")
                .field("role", role)
                .field("phase", phase)
                .field("parts", &parts.len())
                .finish(),
            Self::FunctionCall {
                call_id,
                name,
                arguments,
                opaque_continuation,
            } => formatter
                .debug_struct("FunctionCall")
                .field("call_id", call_id)
                .field("name", name)
                .field("argument_bytes", &arguments.len())
                .field(
                    "opaque_continuation",
                    &opaque_continuation.as_ref().map(|_| "[REDACTED]"),
                )
                .finish(),
            Self::FunctionCallOutput { call_id, output } => formatter
                .debug_struct("FunctionCallOutput")
                .field("call_id", call_id)
                .field("output_bytes", &output.len())
                .finish(),
            Self::Reasoning {
                id,
                summaries,
                encrypted_content,
            } => formatter
                .debug_struct("Reasoning")
                .field("id", id)
                .field("summary_count", &summaries.len())
                .field(
                    "encrypted_content",
                    &encrypted_content.as_ref().map(|_| "[REDACTED]"),
                )
                .finish(),
        }
    }
}

#[derive(Clone)]
pub struct ProviderTool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl fmt::Debug for ProviderTool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderTool")
            .field("name", &self.name)
            .field("description_bytes", &self.description.len())
            .field("parameters", &"[REDACTED]")
            .finish()
    }
}

pub struct OpenCodeResponseRequest {
    opencode_session_id: [u8; 16],
    model: &'static OpenCodeModel,
    estimated_input_tokens: u32,
    maximum_output_tokens: u32,
    input_items: usize,
    tool_count: usize,
    body: Bytes,
}

/// Immutable, bounded tool definitions. Only Gemini's schema lowering needs a
/// cached projection; other encoders borrow the original validated schema.
pub(crate) struct PreparedProviderTools {
    definitions: Vec<ProviderTool>,
    gemini_parameters: OnceLock<Result<Vec<Option<Value>>, ProviderError>>,
}

impl fmt::Debug for PreparedProviderTools {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedProviderTools")
            .field("tool_count", &self.definitions.len())
            .field("gemini_projected", &self.gemini_parameters.get().is_some())
            .finish()
    }
}

impl PreparedProviderTools {
    pub(crate) fn new(definitions: Vec<ProviderTool>) -> Result<Self, ProviderError> {
        validate_tool_definitions(&definitions)?;
        Ok(Self {
            definitions,
            gemini_parameters: OnceLock::new(),
        })
    }

    pub(crate) fn empty() -> &'static Self {
        static EMPTY: PreparedProviderTools = PreparedProviderTools {
            definitions: Vec::new(),
            gemini_parameters: OnceLock::new(),
        };
        &EMPTY
    }

    pub(crate) fn definitions(&self) -> &[ProviderTool] {
        &self.definitions
    }

    fn gemini_parameters(&self) -> Result<&[Option<Value>], ProviderError> {
        self.gemini_parameters
            .get_or_init(|| {
                self.definitions
                    .iter()
                    .map(|tool| gemini_tool_schema(&tool.parameters))
                    .collect()
            })
            .as_ref()
            .map(Vec::as_slice)
            .map_err(|error| *error)
    }
}

struct RequestEncoder<'a> {
    model: &'static OpenCodeModel,
    maximum_output_tokens: u32,
    input: &'a [ProviderInputItem],
    tools: &'a PreparedProviderTools,
}

impl fmt::Debug for OpenCodeResponseRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenCodeResponseRequest")
            .field("service", &self.model.service)
            .field("model", &self.model.id)
            .field("estimated_input_tokens", &self.estimated_input_tokens)
            .field("maximum_output_tokens", &self.maximum_output_tokens)
            .field("input_items", &self.input_items)
            .field("tools", &self.tool_count)
            .finish()
    }
}

impl OpenCodeResponseRequest {
    pub fn new(
        conversation_id: [u8; 16],
        service: OpenCodeService,
        model_id: &str,
        estimated_input_tokens: u32,
        maximum_output_tokens: u32,
        input: Vec<ProviderInputItem>,
        tools: Vec<ProviderTool>,
    ) -> Result<Self, ProviderError> {
        Self::with_prepared_tools(
            conversation_id,
            service,
            model_id,
            estimated_input_tokens,
            maximum_output_tokens,
            input,
            &PreparedProviderTools::new(tools)?,
        )
    }

    pub(crate) fn with_prepared_tools(
        conversation_id: [u8; 16],
        service: OpenCodeService,
        model_id: &str,
        estimated_input_tokens: u32,
        maximum_output_tokens: u32,
        input: Vec<ProviderInputItem>,
        tools: &PreparedProviderTools,
    ) -> Result<Self, ProviderError> {
        let model =
            find_open_code_model(service, model_id).ok_or(ProviderError::UnsupportedModel)?;
        if conversation_id.iter().all(|byte| *byte == 0)
            || estimated_input_tokens == 0
            || estimated_input_tokens > model.maximum_input_tokens
            || maximum_output_tokens == 0
            || maximum_output_tokens > model.maximum_output_tokens
            || estimated_input_tokens
                .checked_add(maximum_output_tokens)
                .is_none_or(|total| {
                    total > model.maximum_input_tokens + model.maximum_output_tokens
                })
        {
            return Err(ProviderError::InvalidRequest);
        }
        validate_input(&input, model)?;
        if !tools.definitions.is_empty() && !model.capabilities.tool_calls {
            return Err(ProviderError::InvalidRequest);
        }
        let digest = Sha256::new()
            .chain_update(OPENCODE_SESSION_FINGERPRINT_CONTEXT)
            .chain_update(conversation_id)
            .finalize();
        let mut opencode_session_id = [0_u8; 16];
        opencode_session_id.copy_from_slice(&digest[..16]);
        let body = RequestEncoder {
            model,
            maximum_output_tokens,
            input: &input,
            tools,
        }
        .encode_body()?;
        Ok(Self {
            opencode_session_id,
            model,
            estimated_input_tokens,
            maximum_output_tokens,
            input_items: input.len(),
            tool_count: tools.definitions.len(),
            body: Bytes::from(body),
        })
    }

    #[must_use]
    pub const fn model(&self) -> &'static OpenCodeModel {
        self.model
    }

    pub(super) fn opencode_session_header(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut value = String::with_capacity(4 + self.opencode_session_id.len() * 2);
        value.push_str("ses_");
        for byte in self.opencode_session_id {
            value.push(char::from(HEX[usize::from(byte >> 4)]));
            value.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        value
    }

    pub(super) fn encoded_body(&self) -> Bytes {
        self.body.clone()
    }
}

impl RequestEncoder<'_> {
    fn encode_body(&self) -> Result<Vec<u8>, ProviderError> {
        let body = match self.model.protocol {
            ProviderProtocol::Responses => self.encode_responses_body()?,
            ProviderProtocol::ChatCompletions => self.encode_chat_completions_body()?,
            ProviderProtocol::AnthropicMessages => self.encode_anthropic_messages_body()?,
            ProviderProtocol::Gemini => self.encode_gemini_body()?,
        };
        if body.len() > MAX_PROVIDER_REQUEST_BYTES {
            return Err(ProviderError::InvalidRequest);
        }
        Ok(body)
    }

    fn encode_responses_body(&self) -> Result<Vec<u8>, ProviderError> {
        let input = self.input.iter().map(WireInputItem::from).collect();
        let tools = self.tools.definitions.iter().map(WireTool::from).collect();
        serde_json::to_vec(&WireRequest {
            model: self.model.id,
            include: self
                .model
                .capabilities
                .reasoning_continuation
                .then_some(["reasoning.encrypted_content"]),
            input,
            tools,
            maximum_output_tokens: self.maximum_output_tokens,
            parallel_tool_calls: false,
            store: false,
            stream: true,
        })
        .map_err(|_| ProviderError::InvalidRequest)
    }

    fn encode_chat_completions_body(&self) -> Result<Vec<u8>, ProviderError> {
        let messages = chat_messages(self.input, self.model.id.starts_with("deepseek-"))?;
        let tools = self
            .tools
            .definitions
            .iter()
            .map(ChatWireTool::from)
            .collect();
        serde_json::to_vec(&ChatWireRequest {
            model: self.model.id,
            messages,
            tools,
            maximum_output_tokens: self.maximum_output_tokens,
            stream: true,
            stream_options: ChatWireStreamOptions {
                include_usage: true,
            },
        })
        .map_err(|_| ProviderError::InvalidRequest)
    }

    fn encode_anthropic_messages_body(&self) -> Result<Vec<u8>, ProviderError> {
        let (system, messages) = anthropic_messages(self.input)?;
        let tools = self
            .tools
            .definitions
            .iter()
            .map(AnthropicWireTool::from)
            .collect();
        serde_json::to_vec(&AnthropicWireRequest {
            model: self.model.id,
            system,
            messages,
            tools,
            maximum_output_tokens: self.maximum_output_tokens,
            stream: true,
        })
        .map_err(|_| ProviderError::InvalidRequest)
    }

    fn encode_gemini_body(&self) -> Result<Vec<u8>, ProviderError> {
        let (system_instruction, contents) = gemini_contents(self.input)?;
        let function_declarations: Vec<_> = self
            .tools
            .definitions
            .iter()
            .zip(self.tools.gemini_parameters()?)
            .map(|(tool, parameters)| GeminiWireFunctionDeclaration {
                name: &tool.name,
                description: &tool.description,
                parameters: parameters.as_ref(),
            })
            .collect();
        let tools = (!function_declarations.is_empty()).then_some([GeminiWireTool {
            function_declarations,
        }]);
        serde_json::to_vec(&GeminiWireRequest {
            contents,
            system_instruction,
            tools,
            generation_config: GeminiWireGenerationConfig {
                max_output_tokens: self.maximum_output_tokens,
            },
        })
        .map_err(|_| ProviderError::InvalidRequest)
    }
}

fn validate_input(input: &[ProviderInputItem], model: &OpenCodeModel) -> Result<(), ProviderError> {
    if input.is_empty() || input.len() > MAX_INPUT_ITEMS {
        return Err(ProviderError::InvalidRequest);
    }
    let mut aggregate_bytes = 0_usize;
    let mut image_count = 0_usize;
    let mut aggregate_image_bytes = 0_usize;
    for item in input {
        let item_bytes = match item {
            ProviderInputItem::Message { role, text, phase } => {
                if text.is_empty()
                    || text.len() > MAX_INPUT_TEXT_BYTES
                    || (phase.is_some() && *role != ProviderMessageRole::Assistant)
                {
                    return Err(ProviderError::InvalidRequest);
                }
                text.len()
            }
            ProviderInputItem::MultimodalMessage { role, parts, phase } => {
                if *role != ProviderMessageRole::User
                    || phase.is_some()
                    || !model.capabilities.image_input
                    || parts.is_empty()
                    || parts.len() > MAX_INPUT_IMAGES * 2 + 1
                {
                    return Err(ProviderError::InvalidRequest);
                }
                let mut item_bytes = 0_usize;
                let mut saw_image = false;
                for part in parts {
                    let bytes = match part {
                        ProviderContentPart::Text(text) => {
                            if text.is_empty() || text.len() > MAX_INPUT_TEXT_BYTES {
                                return Err(ProviderError::InvalidRequest);
                            }
                            text.len()
                        }
                        ProviderContentPart::Image {
                            media_type,
                            width,
                            height,
                            bytes,
                        } => {
                            if bytes.is_empty()
                                || bytes.len() > MAX_INPUT_IMAGE_BYTES
                                || !morons_image::validate_normalized_image(
                                    bytes,
                                    *media_type,
                                    *width,
                                    *height,
                                )
                            {
                                return Err(ProviderError::InvalidRequest);
                            }
                            image_count = image_count
                                .checked_add(1)
                                .ok_or(ProviderError::InvalidRequest)?;
                            aggregate_image_bytes = aggregate_image_bytes
                                .checked_add(bytes.len())
                                .ok_or(ProviderError::InvalidRequest)?;
                            if image_count > MAX_INPUT_IMAGES
                                || aggregate_image_bytes > MAX_AGGREGATE_IMAGE_BYTES
                            {
                                return Err(ProviderError::InvalidRequest);
                            }
                            saw_image = true;
                            bytes
                                .len()
                                .checked_add(2)
                                .and_then(|length| length.checked_div(3))
                                .and_then(|length| length.checked_mul(4))
                                .ok_or(ProviderError::InvalidRequest)?
                        }
                    };
                    item_bytes = item_bytes
                        .checked_add(bytes)
                        .ok_or(ProviderError::InvalidRequest)?;
                }
                if !saw_image {
                    return Err(ProviderError::InvalidRequest);
                }
                item_bytes
            }
            ProviderInputItem::FunctionCall {
                call_id,
                name,
                arguments,
                opaque_continuation,
            } => {
                validate_identifier(call_id, MAX_PROVIDER_CALL_ID_BYTES)?;
                validate_tool_name(name)?;
                if arguments.is_empty()
                    || arguments.len() > MAX_TOOL_ARGUMENT_BYTES
                    || opaque_continuation.as_ref().is_some_and(|continuation| {
                        model.protocol != ProviderProtocol::Gemini
                            || !model.capabilities.reasoning_continuation
                            || continuation.is_empty()
                            || continuation.len() > MAX_ENCRYPTED_REASONING_BYTES
                    })
                {
                    return Err(ProviderError::InvalidRequest);
                }
                let decoded_arguments = parse_strict_value(arguments.as_bytes())
                    .map_err(|_| ProviderError::InvalidRequest)?;
                if !decoded_arguments.is_object() {
                    return Err(ProviderError::InvalidRequest);
                }
                call_id.len()
                    + name.len()
                    + arguments.len()
                    + opaque_continuation.as_ref().map_or(0, String::len)
            }
            ProviderInputItem::FunctionCallOutput { call_id, output } => {
                validate_identifier(call_id, MAX_PROVIDER_CALL_ID_BYTES)?;
                if output.len() > MAX_TOOL_RESULT_BYTES {
                    return Err(ProviderError::InvalidRequest);
                }
                call_id.len() + output.len()
            }
            ProviderInputItem::Reasoning {
                id,
                summaries,
                encrypted_content,
            } => {
                validate_identifier(id, MAX_PROVIDER_CALL_ID_BYTES)?;
                if model.protocol != ProviderProtocol::Responses
                    || (summaries.is_empty() && encrypted_content.is_none())
                    || (encrypted_content.is_some() && !model.capabilities.reasoning_continuation)
                    || summaries.len() > MAX_REASONING_SUMMARIES
                    || summaries
                        .iter()
                        .any(|summary| summary.len() > MAX_REASONING_SUMMARY_BYTES)
                    || encrypted_content.as_ref().is_some_and(|content| {
                        content.is_empty() || content.len() > MAX_ENCRYPTED_REASONING_BYTES
                    })
                {
                    return Err(ProviderError::InvalidRequest);
                }
                id.len()
                    + summaries.iter().map(String::len).sum::<usize>()
                    + encrypted_content.as_ref().map_or(0, String::len)
            }
        };
        aggregate_bytes = aggregate_bytes
            .checked_add(item_bytes)
            .ok_or(ProviderError::InvalidRequest)?;
        if aggregate_bytes > MAX_AGGREGATE_INPUT_BYTES {
            return Err(ProviderError::InvalidRequest);
        }
    }
    Ok(())
}

fn validate_tool_definitions(tools: &[ProviderTool]) -> Result<(), ProviderError> {
    if tools.len() > MAX_TOOL_COUNT {
        return Err(ProviderError::InvalidRequest);
    }
    let mut names = BTreeSet::new();
    for tool in tools {
        validate_tool_name(&tool.name)?;
        if !names.insert(tool.name.as_str())
            || tool.description.is_empty()
            || tool.description.len() > MAX_TOOL_DESCRIPTION_BYTES
            || !tool.parameters.is_object()
        {
            return Err(ProviderError::InvalidRequest);
        }
        let encoded =
            serde_json::to_vec(&tool.parameters).map_err(|_| ProviderError::InvalidRequest)?;
        if encoded.len() > MAX_TOOL_SCHEMA_BYTES {
            return Err(ProviderError::InvalidRequest);
        }
        let mut nodes = 0;
        validate_json_value(&tool.parameters, 0, &mut nodes)?;
    }
    Ok(())
}

fn validate_json_value(
    value: &Value,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), ProviderError> {
    if depth > MAX_TOOL_SCHEMA_DEPTH {
        return Err(ProviderError::InvalidRequest);
    }
    *nodes = nodes.checked_add(1).ok_or(ProviderError::InvalidRequest)?;
    if *nodes > MAX_TOOL_SCHEMA_NODES {
        return Err(ProviderError::InvalidRequest);
    }
    match value {
        Value::Array(values) => {
            for value in values {
                validate_json_value(value, depth + 1, nodes)?;
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                if key.len() > MAX_TOOL_NAME_BYTES {
                    return Err(ProviderError::InvalidRequest);
                }
                validate_json_value(value, depth + 1, nodes)?;
            }
        }
        Value::String(value) if value.len() > MAX_TOOL_SCHEMA_BYTES => {
            return Err(ProviderError::InvalidRequest);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

pub(super) fn validate_tool_name(name: &str) -> Result<(), ProviderError> {
    if name.is_empty()
        || name.len() > MAX_TOOL_NAME_BYTES
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(ProviderError::InvalidRequest);
    }
    Ok(())
}

pub(super) fn validate_identifier(value: &str, maximum_bytes: usize) -> Result<(), ProviderError> {
    if value.is_empty()
        || value.len() > maximum_bytes
        || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(ProviderError::InvalidRequest);
    }
    Ok(())
}

#[derive(Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    include: Option<[&'static str; 1]>,
    input: Vec<WireInputItem<'a>>,
    tools: Vec<WireTool<'a>>,
    #[serde(rename = "max_output_tokens")]
    maximum_output_tokens: u32,
    parallel_tool_calls: bool,
    store: bool,
    stream: bool,
}

#[derive(Serialize)]
#[serde(untagged)]
enum WireInputItem<'a> {
    Message(WireMessage<'a>),
    FunctionCall(WireFunctionCall<'a>),
    FunctionCallOutput(WireFunctionCallOutput<'a>),
    Reasoning(WireReasoning<'a>),
}

impl<'a> From<&'a ProviderInputItem> for WireInputItem<'a> {
    fn from(item: &'a ProviderInputItem) -> Self {
        match item {
            ProviderInputItem::Message { role, text, phase } => Self::Message(WireMessage {
                role: *role,
                content: WireMessageContent::Text(text),
                phase: *phase,
            }),
            ProviderInputItem::MultimodalMessage { role, parts, phase } => {
                Self::Message(WireMessage {
                    role: *role,
                    content: WireMessageContent::Parts(
                        parts
                            .iter()
                            .map(|part| match part {
                                ProviderContentPart::Text(text) => {
                                    WireContentPart::InputText { text }
                                }
                                ProviderContentPart::Image {
                                    media_type, bytes, ..
                                } => WireContentPart::InputImage {
                                    image_url: format!(
                                        "data:{};base64,{}",
                                        media_type.as_str(),
                                        morons_image::encode_base64(bytes)
                                    ),
                                    detail: "auto",
                                },
                            })
                            .collect(),
                    ),
                    phase: *phase,
                })
            }
            ProviderInputItem::FunctionCall {
                call_id,
                name,
                arguments,
                opaque_continuation: _,
            } => Self::FunctionCall(WireFunctionCall {
                item_type: "function_call",
                call_id,
                name,
                arguments,
            }),
            ProviderInputItem::FunctionCallOutput { call_id, output } => {
                Self::FunctionCallOutput(WireFunctionCallOutput {
                    item_type: "function_call_output",
                    call_id,
                    output,
                })
            }
            ProviderInputItem::Reasoning {
                id,
                summaries,
                encrypted_content,
            } => Self::Reasoning(WireReasoning {
                item_type: "reasoning",
                id,
                summary: summaries
                    .iter()
                    .map(|text| WireReasoningSummary {
                        summary_type: "summary_text",
                        text,
                    })
                    .collect(),
                encrypted_content: encrypted_content.as_deref(),
            }),
        }
    }
}

#[derive(Serialize)]
struct WireMessage<'a> {
    role: ProviderMessageRole,
    content: WireMessageContent<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<ProviderMessagePhase>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum WireMessageContent<'a> {
    Text(&'a str),
    Parts(Vec<WireContentPart<'a>>),
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireContentPart<'a> {
    InputText {
        text: &'a str,
    },
    InputImage {
        image_url: String,
        detail: &'static str,
    },
}

#[derive(Serialize)]
struct WireFunctionCall<'a> {
    #[serde(rename = "type")]
    item_type: &'static str,
    call_id: &'a str,
    name: &'a str,
    arguments: &'a str,
}

#[derive(Serialize)]
struct WireFunctionCallOutput<'a> {
    #[serde(rename = "type")]
    item_type: &'static str,
    call_id: &'a str,
    output: &'a str,
}

#[derive(Serialize)]
struct WireReasoning<'a> {
    #[serde(rename = "type")]
    item_type: &'static str,
    id: &'a str,
    summary: Vec<WireReasoningSummary<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    encrypted_content: Option<&'a str>,
}

#[derive(Serialize)]
struct WireReasoningSummary<'a> {
    #[serde(rename = "type")]
    summary_type: &'static str,
    text: &'a str,
}

#[derive(Serialize)]
struct WireTool<'a> {
    #[serde(rename = "type")]
    tool_type: &'static str,
    name: &'a str,
    description: &'a str,
    parameters: &'a Value,
    strict: bool,
}

impl<'a> From<&'a ProviderTool> for WireTool<'a> {
    fn from(tool: &'a ProviderTool) -> Self {
        Self {
            tool_type: "function",
            name: &tool.name,
            description: &tool.description,
            parameters: &tool.parameters,
            // Reviewed schemas contain optional fields; Morons validates returned arguments.
            strict: false,
        }
    }
}

#[derive(Serialize)]
struct ChatWireRequest<'a> {
    model: &'a str,
    messages: Vec<ChatWireMessage<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ChatWireTool<'a>>,
    #[serde(rename = "max_tokens")]
    maximum_output_tokens: u32,
    stream: bool,
    stream_options: ChatWireStreamOptions,
}

#[derive(Serialize)]
struct ChatWireStreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct ChatWireMessage<'a> {
    role: &'static str,
    content: Option<ChatWireMessageContent<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ChatWireToolCall<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<&'static str>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ChatWireMessageContent<'a> {
    Text(&'a str),
    Parts(Vec<ChatWireContentPart<'a>>),
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ChatWireContentPart<'a> {
    Text { text: &'a str },
    ImageUrl { image_url: ChatWireImageUrl },
}

#[derive(Serialize)]
struct ChatWireImageUrl {
    url: String,
}

#[derive(Serialize)]
struct ChatWireToolCall<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    call_type: &'static str,
    function: ChatWireToolCallFunction<'a>,
}

#[derive(Serialize)]
struct ChatWireToolCallFunction<'a> {
    name: &'a str,
    arguments: &'a str,
}

fn chat_messages(
    input: &[ProviderInputItem],
    requires_reasoning_content: bool,
) -> Result<Vec<ChatWireMessage<'_>>, ProviderError> {
    let mut messages = Vec::new();
    let mut index = 0_usize;
    while index < input.len() {
        match &input[index] {
            ProviderInputItem::Message {
                role,
                text,
                phase: _,
            } => {
                messages.push(ChatWireMessage {
                    role: match role {
                        ProviderMessageRole::Developer => "system",
                        ProviderMessageRole::User => "user",
                        ProviderMessageRole::Assistant => "assistant",
                    },
                    content: Some(ChatWireMessageContent::Text(text)),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: (*role == ProviderMessageRole::Assistant
                        && requires_reasoning_content)
                        .then_some(""),
                });
                index += 1;
            }
            ProviderInputItem::MultimodalMessage { parts, .. } => {
                messages.push(ChatWireMessage {
                    role: "user",
                    content: Some(ChatWireMessageContent::Parts(
                        parts
                            .iter()
                            .map(|part| match part {
                                ProviderContentPart::Text(text) => {
                                    ChatWireContentPart::Text { text }
                                }
                                ProviderContentPart::Image {
                                    media_type, bytes, ..
                                } => ChatWireContentPart::ImageUrl {
                                    image_url: ChatWireImageUrl {
                                        url: format!(
                                            "data:{};base64,{}",
                                            media_type.as_str(),
                                            morons_image::encode_base64(bytes)
                                        ),
                                    },
                                },
                            })
                            .collect(),
                    )),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                });
                index += 1;
            }
            ProviderInputItem::FunctionCall { .. } => {
                let mut calls = Vec::new();
                while let Some(ProviderInputItem::FunctionCall {
                    call_id,
                    name,
                    arguments,
                    opaque_continuation: _,
                }) = input.get(index)
                {
                    calls.push(ChatWireToolCall {
                        id: call_id,
                        call_type: "function",
                        function: ChatWireToolCallFunction { name, arguments },
                    });
                    index += 1;
                }
                messages.push(ChatWireMessage {
                    role: "assistant",
                    content: None,
                    tool_calls: Some(calls),
                    tool_call_id: None,
                    reasoning_content: requires_reasoning_content.then_some(""),
                });
            }
            ProviderInputItem::FunctionCallOutput { call_id, output } => {
                messages.push(ChatWireMessage {
                    role: "tool",
                    content: Some(ChatWireMessageContent::Text(output)),
                    tool_calls: None,
                    tool_call_id: Some(call_id),
                    reasoning_content: None,
                });
                index += 1;
            }
            ProviderInputItem::Reasoning { .. } => return Err(ProviderError::InvalidRequest),
        }
    }
    Ok(messages)
}

#[derive(Serialize)]
struct ChatWireTool<'a> {
    #[serde(rename = "type")]
    tool_type: &'static str,
    function: ChatWireToolFunction<'a>,
}

#[derive(Serialize)]
struct ChatWireToolFunction<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a Value,
}

impl<'a> From<&'a ProviderTool> for ChatWireTool<'a> {
    fn from(tool: &'a ProviderTool) -> Self {
        Self {
            tool_type: "function",
            function: ChatWireToolFunction {
                name: &tool.name,
                description: &tool.description,
                parameters: &tool.parameters,
            },
        }
    }
}

#[derive(Serialize)]
struct AnthropicWireRequest<'a> {
    model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicWireMessage<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicWireTool<'a>>,
    #[serde(rename = "max_tokens")]
    maximum_output_tokens: u32,
    stream: bool,
}

#[derive(Serialize)]
struct AnthropicWireMessage<'a> {
    role: &'static str,
    content: Vec<AnthropicWireContent<'a>>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicWireContent<'a> {
    Text {
        text: &'a str,
    },
    Image {
        source: AnthropicWireImageSource,
    },
    ToolUse {
        id: &'a str,
        name: &'a str,
        input: Value,
    },
    ToolResult {
        tool_use_id: &'a str,
        content: &'a str,
    },
}

#[derive(Serialize)]
struct AnthropicWireImageSource {
    #[serde(rename = "type")]
    source_type: &'static str,
    media_type: &'static str,
    data: String,
}

#[derive(Serialize)]
struct AnthropicWireTool<'a> {
    name: &'a str,
    description: &'a str,
    input_schema: &'a Value,
}

impl<'a> From<&'a ProviderTool> for AnthropicWireTool<'a> {
    fn from(tool: &'a ProviderTool) -> Self {
        Self {
            name: &tool.name,
            description: &tool.description,
            input_schema: &tool.parameters,
        }
    }
}

fn anthropic_messages(
    input: &[ProviderInputItem],
) -> Result<(Option<String>, Vec<AnthropicWireMessage<'_>>), ProviderError> {
    let mut system_parts = Vec::new();
    let mut messages = Vec::new();
    let mut index = 0_usize;
    while index < input.len() {
        match &input[index] {
            ProviderInputItem::Message {
                role: ProviderMessageRole::Developer,
                text,
                phase: _,
            } => {
                if !messages.is_empty() {
                    return Err(ProviderError::InvalidRequest);
                }
                system_parts.push(text.as_str());
                index += 1;
            }
            ProviderInputItem::Message {
                role: ProviderMessageRole::User,
                text,
                phase: _,
            } => {
                messages.push(AnthropicWireMessage {
                    role: "user",
                    content: vec![AnthropicWireContent::Text { text }],
                });
                index += 1;
            }
            ProviderInputItem::Message {
                role: ProviderMessageRole::Assistant,
                text,
                phase: _,
            } => {
                let mut content = vec![AnthropicWireContent::Text { text }];
                index += 1;
                append_anthropic_tool_uses(input, &mut index, &mut content)?;
                messages.push(AnthropicWireMessage {
                    role: "assistant",
                    content,
                });
            }
            ProviderInputItem::MultimodalMessage { parts, .. } => {
                let content = parts
                    .iter()
                    .map(|part| match part {
                        ProviderContentPart::Text(text) => AnthropicWireContent::Text { text },
                        ProviderContentPart::Image {
                            media_type, bytes, ..
                        } => AnthropicWireContent::Image {
                            source: AnthropicWireImageSource {
                                source_type: "base64",
                                media_type: media_type.as_str(),
                                data: morons_image::encode_base64(bytes),
                            },
                        },
                    })
                    .collect();
                messages.push(AnthropicWireMessage {
                    role: "user",
                    content,
                });
                index += 1;
            }
            ProviderInputItem::FunctionCall { .. } => {
                let mut content = Vec::new();
                append_anthropic_tool_uses(input, &mut index, &mut content)?;
                messages.push(AnthropicWireMessage {
                    role: "assistant",
                    content,
                });
            }
            ProviderInputItem::FunctionCallOutput { .. } => {
                let mut content = Vec::new();
                while let Some(ProviderInputItem::FunctionCallOutput { call_id, output }) =
                    input.get(index)
                {
                    content.push(AnthropicWireContent::ToolResult {
                        tool_use_id: call_id,
                        content: output,
                    });
                    index += 1;
                }
                messages.push(AnthropicWireMessage {
                    role: "user",
                    content,
                });
            }
            ProviderInputItem::Reasoning { .. } => return Err(ProviderError::InvalidRequest),
        }
    }
    if messages.is_empty() {
        return Err(ProviderError::InvalidRequest);
    }
    Ok((
        (!system_parts.is_empty()).then(|| system_parts.join("\n\n")),
        messages,
    ))
}

fn append_anthropic_tool_uses<'a>(
    input: &'a [ProviderInputItem],
    index: &mut usize,
    content: &mut Vec<AnthropicWireContent<'a>>,
) -> Result<(), ProviderError> {
    while let Some(ProviderInputItem::FunctionCall {
        call_id,
        name,
        arguments,
        opaque_continuation: _,
    }) = input.get(*index)
    {
        let arguments =
            parse_strict_value(arguments.as_bytes()).map_err(|_| ProviderError::InvalidRequest)?;
        content.push(AnthropicWireContent::ToolUse {
            id: call_id,
            name,
            input: arguments,
        });
        *index += 1;
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiWireRequest<'a> {
    contents: Vec<GeminiWireContent<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiWireSystemInstruction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<[GeminiWireTool<'a>; 1]>,
    generation_config: GeminiWireGenerationConfig,
}

#[derive(Serialize)]
struct GeminiWireSystemInstruction {
    parts: [GeminiWireOwnedTextPart; 1],
}

#[derive(Serialize)]
struct GeminiWireOwnedTextPart {
    text: String,
}

#[derive(Serialize)]
struct GeminiWireContent<'a> {
    role: &'static str,
    parts: Vec<GeminiWirePart<'a>>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum GeminiWirePart<'a> {
    Text {
        text: &'a str,
    },
    InlineData {
        #[serde(rename = "inlineData")]
        inline_data: GeminiWireInlineData,
    },
    FunctionCall {
        #[serde(rename = "functionCall")]
        function_call: GeminiWireFunctionCall<'a>,
        #[serde(rename = "thoughtSignature", skip_serializing_if = "Option::is_none")]
        thought_signature: Option<&'a str>,
    },
    FunctionResponse {
        #[serde(rename = "functionResponse")]
        function_response: GeminiWireFunctionResponse<'a>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiWireInlineData {
    mime_type: &'static str,
    data: String,
}

#[derive(Serialize)]
struct GeminiWireFunctionCall<'a> {
    name: &'a str,
    args: Value,
}

#[derive(Serialize)]
struct GeminiWireFunctionResponse<'a> {
    name: &'a str,
    response: GeminiWireFunctionResponseBody<'a>,
}

#[derive(Serialize)]
struct GeminiWireFunctionResponseBody<'a> {
    name: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiWireTool<'a> {
    function_declarations: Vec<GeminiWireFunctionDeclaration<'a>>,
}

#[derive(Serialize)]
struct GeminiWireFunctionDeclaration<'a> {
    name: &'a str,
    description: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    parameters: Option<&'a Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiWireGenerationConfig {
    max_output_tokens: u32,
}

fn gemini_contents(
    input: &[ProviderInputItem],
) -> Result<
    (
        Option<GeminiWireSystemInstruction>,
        Vec<GeminiWireContent<'_>>,
    ),
    ProviderError,
> {
    let mut system_parts = Vec::new();
    let mut contents = Vec::new();
    let mut call_names = BTreeMap::new();
    let mut index = 0_usize;
    while index < input.len() {
        match &input[index] {
            ProviderInputItem::Message {
                role: ProviderMessageRole::Developer,
                text,
                phase: _,
            } => {
                if !contents.is_empty() {
                    return Err(ProviderError::InvalidRequest);
                }
                system_parts.push(text.as_str());
                index += 1;
            }
            ProviderInputItem::Message {
                role: ProviderMessageRole::User,
                text,
                phase: _,
            } => {
                contents.push(GeminiWireContent {
                    role: "user",
                    parts: vec![GeminiWirePart::Text { text }],
                });
                index += 1;
            }
            ProviderInputItem::Message {
                role: ProviderMessageRole::Assistant,
                text,
                phase: _,
            } => {
                let mut parts = vec![GeminiWirePart::Text { text }];
                index += 1;
                append_gemini_function_calls(input, &mut index, &mut parts, &mut call_names)?;
                contents.push(GeminiWireContent {
                    role: "model",
                    parts,
                });
            }
            ProviderInputItem::MultimodalMessage { parts, .. } => {
                contents.push(GeminiWireContent {
                    role: "user",
                    parts: parts
                        .iter()
                        .map(|part| match part {
                            ProviderContentPart::Text(text) => GeminiWirePart::Text { text },
                            ProviderContentPart::Image {
                                media_type, bytes, ..
                            } => GeminiWirePart::InlineData {
                                inline_data: GeminiWireInlineData {
                                    mime_type: media_type.as_str(),
                                    data: morons_image::encode_base64(bytes),
                                },
                            },
                        })
                        .collect(),
                });
                index += 1;
            }
            ProviderInputItem::FunctionCall { .. } => {
                let mut parts = Vec::new();
                append_gemini_function_calls(input, &mut index, &mut parts, &mut call_names)?;
                contents.push(GeminiWireContent {
                    role: "model",
                    parts,
                });
            }
            ProviderInputItem::FunctionCallOutput { .. } => {
                let mut parts = Vec::new();
                while let Some(ProviderInputItem::FunctionCallOutput { call_id, output }) =
                    input.get(index)
                {
                    let name = call_names
                        .get(call_id.as_str())
                        .copied()
                        .ok_or(ProviderError::InvalidRequest)?;
                    parts.push(GeminiWirePart::FunctionResponse {
                        function_response: GeminiWireFunctionResponse {
                            name,
                            response: GeminiWireFunctionResponseBody {
                                name,
                                content: output,
                            },
                        },
                    });
                    index += 1;
                }
                contents.push(GeminiWireContent {
                    role: "user",
                    parts,
                });
            }
            ProviderInputItem::Reasoning { .. } => return Err(ProviderError::InvalidRequest),
        }
    }
    if contents.is_empty() {
        return Err(ProviderError::InvalidRequest);
    }
    Ok((
        (!system_parts.is_empty()).then(|| GeminiWireSystemInstruction {
            parts: [GeminiWireOwnedTextPart {
                text: system_parts.join("\n\n"),
            }],
        }),
        contents,
    ))
}

fn append_gemini_function_calls<'a>(
    input: &'a [ProviderInputItem],
    index: &mut usize,
    parts: &mut Vec<GeminiWirePart<'a>>,
    call_names: &mut BTreeMap<&'a str, &'a str>,
) -> Result<(), ProviderError> {
    while let Some(ProviderInputItem::FunctionCall {
        call_id,
        name,
        arguments,
        opaque_continuation,
    }) = input.get(*index)
    {
        if call_names.insert(call_id, name).is_some() {
            return Err(ProviderError::InvalidRequest);
        }
        let args =
            parse_strict_value(arguments.as_bytes()).map_err(|_| ProviderError::InvalidRequest)?;
        parts.push(GeminiWirePart::FunctionCall {
            function_call: GeminiWireFunctionCall { name, args },
            thought_signature: opaque_continuation.as_deref(),
        });
        *index += 1;
    }
    Ok(())
}

fn gemini_tool_schema(schema: &Value) -> Result<Option<Value>, ProviderError> {
    let object = schema.as_object().ok_or(ProviderError::InvalidRequest)?;
    let empty_object = object.get("type").and_then(Value::as_str) == Some("object")
        && object
            .get("properties")
            .and_then(Value::as_object)
            .is_none_or(serde_json::Map::is_empty)
        && !object
            .get("additionalProperties")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    if empty_object {
        Ok(None)
    } else {
        gemini_project_schema(schema)
    }
}

fn gemini_project_schema(schema: &Value) -> Result<Option<Value>, ProviderError> {
    let object = schema.as_object().ok_or(ProviderError::InvalidRequest)?;
    let mut result = serde_json::Map::new();
    for key in ["description", "format", "minLength"] {
        if let Some(value) = object.get(key) {
            result.insert(key.to_owned(), value.clone());
        }
    }

    let schema_type = object.get("type");
    let nullable = schema_type
        .and_then(Value::as_array)
        .is_some_and(|types| types.iter().any(|value| value.as_str() == Some("null")));
    let projected_type = match schema_type {
        Some(Value::String(value)) => Some(value.clone()),
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(Value::as_str)
            .find(|value| *value != "null")
            .map(str::to_owned),
        Some(_) => return Err(ProviderError::InvalidRequest),
        None => None,
    };

    let raw_enum = object
        .get("const")
        .map(|value| vec![value.clone()])
        .or_else(|| object.get("enum").and_then(Value::as_array).cloned());
    let numeric_enum =
        matches!(projected_type.as_deref(), Some("integer" | "number")) && raw_enum.is_some();
    if let Some(schema_type) = projected_type {
        result.insert(
            "type".to_owned(),
            Value::String(if numeric_enum {
                "string".to_owned()
            } else {
                schema_type
            }),
        );
    }
    if nullable {
        result.insert("nullable".to_owned(), Value::Bool(true));
    }
    if let Some(values) = raw_enum {
        result.insert(
            "enum".to_owned(),
            Value::Array(if numeric_enum {
                values
                    .into_iter()
                    .map(|value| match value {
                        Value::String(value) => Ok(Value::String(value)),
                        Value::Number(value) => Ok(Value::String(value.to_string())),
                        Value::Bool(value) => Ok(Value::String(value.to_string())),
                        Value::Null => Ok(Value::String("null".to_owned())),
                        Value::Array(_) | Value::Object(_) => Err(ProviderError::InvalidRequest),
                    })
                    .collect::<Result<Vec<_>, ProviderError>>()?
            } else {
                values
            }),
        );
    }

    let allows_properties = !matches!(
        result.get("type").and_then(Value::as_str),
        Some(schema_type) if schema_type != "object"
    ) || ["anyOf", "oneOf", "allOf"]
        .iter()
        .any(|key| object.contains_key(*key));
    if allows_properties && let Some(properties) = object.get("properties") {
        let properties = properties
            .as_object()
            .ok_or(ProviderError::InvalidRequest)?;
        let mut projected = serde_json::Map::new();
        for (name, value) in properties {
            if let Some(value) = gemini_project_schema(value)? {
                projected.insert(name.clone(), value);
            }
        }
        if !projected.is_empty() {
            let required = object
                .get("required")
                .map(|value| {
                    Ok(value
                        .as_array()
                        .ok_or(ProviderError::InvalidRequest)?
                        .iter()
                        .filter_map(Value::as_str)
                        .filter(|name| projected.contains_key(*name))
                        .map(|name| Value::String(name.to_owned()))
                        .collect::<Vec<_>>())
                })
                .transpose()?
                .unwrap_or_default();
            result.insert("properties".to_owned(), Value::Object(projected));
            if !required.is_empty() {
                result.insert("required".to_owned(), Value::Array(required));
            }
        }
    }

    if result.get("type").and_then(Value::as_str) == Some("array") {
        let items = match object.get("items") {
            Some(Value::Array(values)) => Value::Array(
                values
                    .iter()
                    .map(gemini_project_schema)
                    .collect::<Result<Vec<_>, ProviderError>>()?
                    .into_iter()
                    .flatten()
                    .collect(),
            ),
            Some(value) => gemini_project_schema(value)?
                .unwrap_or_else(|| serde_json::json!({"type": "string"})),
            None => serde_json::json!({"type": "string"}),
        };
        result.insert("items".to_owned(), items);
    }

    for key in ["allOf", "anyOf", "oneOf"] {
        if let Some(values) = object.get(key) {
            let values = values
                .as_array()
                .ok_or(ProviderError::InvalidRequest)?
                .iter()
                .map(gemini_project_schema)
                .collect::<Result<Vec<_>, ProviderError>>()?
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            if !values.is_empty() {
                result.insert(key.to_owned(), Value::Array(values));
            }
        }
    }

    Ok((!result.is_empty()).then_some(Value::Object(result)))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "request/prepared_tests.rs"]
mod prepared_tests;
