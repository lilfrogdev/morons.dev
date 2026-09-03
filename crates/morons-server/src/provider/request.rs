use std::{collections::BTreeSet, fmt};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::{
    OpenCodeModel, OpenCodeService, ProviderError, find_open_code_model, json::parse_strict_value,
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
            } => formatter
                .debug_struct("FunctionCall")
                .field("call_id", call_id)
                .field("name", name)
                .field("argument_bytes", &arguments.len())
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
    input: Vec<ProviderInputItem>,
    tools: Vec<ProviderTool>,
}

impl fmt::Debug for OpenCodeResponseRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenCodeResponseRequest")
            .field("service", &self.model.service)
            .field("model", &self.model.id)
            .field("estimated_input_tokens", &self.estimated_input_tokens)
            .field("maximum_output_tokens", &self.maximum_output_tokens)
            .field("input_items", &self.input.len())
            .field("tools", &self.tools.len())
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
        validate_tools(&tools, model)?;
        let digest = Sha256::new()
            .chain_update(OPENCODE_SESSION_FINGERPRINT_CONTEXT)
            .chain_update(conversation_id)
            .finalize();
        let mut opencode_session_id = [0_u8; 16];
        opencode_session_id.copy_from_slice(&digest[..16]);
        let request = Self {
            opencode_session_id,
            model,
            estimated_input_tokens,
            maximum_output_tokens,
            input,
            tools,
        };
        request.encode_body()?;
        Ok(request)
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

    pub(super) fn encode_body(&self) -> Result<Vec<u8>, ProviderError> {
        let input = self.input.iter().map(WireInputItem::from).collect();
        let tools = self.tools.iter().map(WireTool::from).collect();
        let body = serde_json::to_vec(&WireRequest {
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
        .map_err(|_| ProviderError::InvalidRequest)?;
        if body.len() > MAX_PROVIDER_REQUEST_BYTES {
            return Err(ProviderError::InvalidRequest);
        }
        Ok(body)
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
            } => {
                validate_identifier(call_id, MAX_PROVIDER_CALL_ID_BYTES)?;
                validate_tool_name(name)?;
                if arguments.is_empty() || arguments.len() > MAX_TOOL_ARGUMENT_BYTES {
                    return Err(ProviderError::InvalidRequest);
                }
                let decoded_arguments = parse_strict_value(arguments.as_bytes())
                    .map_err(|_| ProviderError::InvalidRequest)?;
                if !decoded_arguments.is_object() {
                    return Err(ProviderError::InvalidRequest);
                }
                call_id.len() + name.len() + arguments.len()
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
                if (summaries.is_empty() && encrypted_content.is_none())
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

fn validate_tools(tools: &[ProviderTool], model: &OpenCodeModel) -> Result<(), ProviderError> {
    if tools.len() > MAX_TOOL_COUNT || (!tools.is_empty() && !model.capabilities.tool_calls) {
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

#[cfg(test)]
mod tests;
