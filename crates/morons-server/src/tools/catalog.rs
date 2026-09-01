use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    MAX_FILE_BYTES, MAX_READ_LINES, MAX_REPLACEMENT_BYTES, MAX_REPLACEMENTS,
    MAX_SEARCH_QUERY_BYTES, MAX_TOOL_CALLS_PER_TURN, TextReplacement, ToolInput, ToolKind,
    ValidatedProviderCall, WorktreePath,
};
use crate::provider::{ProviderTool, ProviderToolCall, json::parse_strict_value};

pub(crate) const TOOL_CATALOG_VERSION: u16 = 1;

const DEVELOPER_INSTRUCTION: &str = "You are operating in an isolated mutable repository worktree. Use only the offered structured tools. Every path is slash-separated and relative to the worktree; use `.` only for read-only directory-scoped tools. Read a complete file digest before editing it. Never assume that a tool succeeded until its committed result says so.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolCallValidationError {
    InvalidProviderOutput,
    ResourceLimit,
}

pub(crate) fn developer_instruction() -> &'static str {
    DEVELOPER_INSTRUCTION
}

pub(crate) fn provider_tools() -> Vec<ProviderTool> {
    vec![
        ProviderTool {
            name: ToolKind::ListDirectory.name().to_owned(),
            description: "List one bounded, byte-sorted page of ordinary children in a worktree directory.".to_owned(),
            parameters: object_schema(
                json!({
                    "path": {"type": "string", "maxLength": 1024},
                    "after": {"type": ["string", "null"], "maxLength": 255}
                }),
                &["path", "after"],
            ),
        },
        ProviderTool {
            name: ToolKind::ReadFile.name().to_owned(),
            description: "Read a bounded one-indexed UTF-8 line window and return the complete file SHA-256 digest.".to_owned(),
            parameters: object_schema(
                json!({
                    "path": {"type": "string", "maxLength": 1024},
                    "start_line": {"type": "integer", "minimum": 1, "maximum": 4294967295_u64},
                    "line_count": {"type": "integer", "minimum": 1, "maximum": MAX_READ_LINES}
                }),
                &["path", "start_line", "line_count"],
            ),
        },
        ProviderTool {
            name: ToolKind::SearchText.name().to_owned(),
            description: "Search for bounded literal UTF-8 text beneath one worktree directory without regex, glob, or ignore-file semantics.".to_owned(),
            parameters: object_schema(
                json!({
                    "path": {"type": "string", "maxLength": 1024},
                    "query": {"type": "string", "minLength": 1, "maxLength": MAX_SEARCH_QUERY_BYTES}
                }),
                &["path", "query"],
            ),
        },
        ProviderTool {
            name: ToolKind::EditFile.name().to_owned(),
            description: "Atomically apply exact unique non-overlapping replacements to an existing UTF-8 file after a complete-file digest precondition.".to_owned(),
            parameters: object_schema(
                json!({
                    "path": {"type": "string", "maxLength": 1024},
                    "expected_sha256": {"type": "string", "minLength": 64, "maxLength": 64},
                    "replacements": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_REPLACEMENTS,
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "old_text": {"type": "string", "maxLength": MAX_REPLACEMENT_BYTES},
                                "new_text": {"type": "string", "maxLength": MAX_REPLACEMENT_BYTES}
                            },
                            "required": ["old_text", "new_text"]
                        }
                    }
                }),
                &["path", "expected_sha256", "replacements"],
            ),
        },
        ProviderTool {
            name: ToolKind::CreateFile.name().to_owned(),
            description: "Atomically create one new UTF-8 file without replacing an existing child or creating missing parents.".to_owned(),
            parameters: object_schema(
                json!({
                    "path": {"type": "string", "maxLength": 1024},
                    "content": {"type": "string", "maxLength": MAX_FILE_BYTES}
                }),
                &["path", "content"],
            ),
        },
        ProviderTool {
            name: ToolKind::CreateDirectory.name().to_owned(),
            description: "Create one empty directory without replacing an existing child or creating missing parents.".to_owned(),
            parameters: object_schema(
                json!({"path": {"type": "string", "maxLength": 1024}}),
                &["path"],
            ),
        },
    ]
}

pub(crate) fn validate_canonical_input(input: &ToolInput) -> bool {
    input
        .provider_arguments()
        .ok()
        .and_then(|arguments| parse_strict_value(arguments.as_bytes()).ok())
        .and_then(|value| parse_input(input.kind().name(), value).ok())
        .as_ref()
        == Some(input)
}

pub(crate) fn parse_provider_calls(
    calls: Vec<ProviderToolCall>,
) -> Result<Vec<ValidatedProviderCall>, ToolCallValidationError> {
    if calls.is_empty() {
        return Err(ToolCallValidationError::InvalidProviderOutput);
    }
    if calls.len() > MAX_TOOL_CALLS_PER_TURN {
        return Err(ToolCallValidationError::ResourceLimit);
    }
    let mut identifiers = BTreeSet::new();
    calls
        .into_iter()
        .map(|call| {
            if !identifiers.insert(call.provider_call_id.clone()) {
                return Err(ToolCallValidationError::InvalidProviderOutput);
            }
            let value = parse_strict_value(call.arguments.as_bytes())
                .map_err(|_| ToolCallValidationError::InvalidProviderOutput)?;
            let input = parse_input(&call.name, value)?;
            Ok(ValidatedProviderCall {
                provider_call_id: call.provider_call_id,
                input,
            })
        })
        .collect()
}

fn parse_input(name: &str, value: Value) -> Result<ToolInput, ToolCallValidationError> {
    match name {
        "list_directory" => {
            require_fields(&value, &["path", "after"])?;
            let arguments: ListDirectory = decode(value)?;
            let path = WorktreePath::parse(&arguments.path, true).map_err(invalid)?;
            if let Some(after) = arguments.after.as_deref() {
                validate_child_name(after)?;
            }
            Ok(ToolInput::ListDirectory {
                path,
                after: arguments.after,
            })
        }
        "read_file" => {
            require_fields(&value, &["path", "start_line", "line_count"])?;
            let arguments: ReadFile = decode(value)?;
            if arguments.start_line == 0
                || arguments.line_count == 0
                || arguments.line_count > MAX_READ_LINES
            {
                return Err(ToolCallValidationError::InvalidProviderOutput);
            }
            Ok(ToolInput::ReadFile {
                path: WorktreePath::parse(&arguments.path, false).map_err(invalid)?,
                start_line: arguments.start_line,
                line_count: arguments.line_count,
            })
        }
        "search_text" => {
            require_fields(&value, &["path", "query"])?;
            let arguments: SearchText = decode(value)?;
            if arguments.query.is_empty()
                || arguments.query.len() > MAX_SEARCH_QUERY_BYTES
                || arguments.query.contains(['\n', '\r', '\0'])
            {
                return Err(ToolCallValidationError::InvalidProviderOutput);
            }
            Ok(ToolInput::SearchText {
                path: WorktreePath::parse(&arguments.path, true).map_err(invalid)?,
                query: arguments.query,
            })
        }
        "edit_file" => {
            require_fields(&value, &["path", "expected_sha256", "replacements"])?;
            let arguments: EditFile = decode(value)?;
            if !valid_digest(&arguments.expected_sha256)
                || arguments.replacements.is_empty()
                || arguments.replacements.len() > MAX_REPLACEMENTS
            {
                return Err(ToolCallValidationError::InvalidProviderOutput);
            }
            let replacement_bytes =
                arguments
                    .replacements
                    .iter()
                    .try_fold(0_usize, |total, replacement| {
                        total
                            .checked_add(replacement.old_text.len())?
                            .checked_add(replacement.new_text.len())
                    });
            if replacement_bytes.is_none_or(|bytes| bytes > MAX_REPLACEMENT_BYTES) {
                return Err(ToolCallValidationError::ResourceLimit);
            }
            Ok(ToolInput::EditFile {
                path: WorktreePath::parse(&arguments.path, false).map_err(invalid)?,
                expected_sha256: arguments.expected_sha256,
                replacements: arguments.replacements,
            })
        }
        "create_file" => {
            require_fields(&value, &["path", "content"])?;
            let arguments: CreateFile = decode(value)?;
            if arguments.content.len() as u64 > MAX_FILE_BYTES {
                return Err(ToolCallValidationError::ResourceLimit);
            }
            Ok(ToolInput::CreateFile {
                path: WorktreePath::parse(&arguments.path, false).map_err(invalid)?,
                content: arguments.content,
            })
        }
        "create_directory" => {
            require_fields(&value, &["path"])?;
            let arguments: CreateDirectory = decode(value)?;
            Ok(ToolInput::CreateDirectory {
                path: WorktreePath::parse(&arguments.path, false).map_err(invalid)?,
            })
        }
        _ => Err(ToolCallValidationError::InvalidProviderOutput),
    }
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required
    })
}

fn require_fields(value: &Value, fields: &[&str]) -> Result<(), ToolCallValidationError> {
    let object = value
        .as_object()
        .ok_or(ToolCallValidationError::InvalidProviderOutput)?;
    if object.len() != fields.len() || fields.iter().any(|field| !object.contains_key(*field)) {
        return Err(ToolCallValidationError::InvalidProviderOutput);
    }
    Ok(())
}

fn decode<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, ToolCallValidationError> {
    serde_json::from_value(value).map_err(|_| ToolCallValidationError::InvalidProviderOutput)
}

fn validate_child_name(value: &str) -> Result<(), ToolCallValidationError> {
    let path = WorktreePath::parse(value, false).map_err(invalid)?;
    if path.components().count() != 1 {
        return Err(ToolCallValidationError::InvalidProviderOutput);
    }
    Ok(())
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

const fn invalid(_: super::ToolErrorKind) -> ToolCallValidationError {
    ToolCallValidationError::InvalidProviderOutput
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListDirectory {
    path: String,
    after: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadFile {
    path: String,
    start_line: u32,
    line_count: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchText {
    path: String,
    query: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditFile {
    path: String,
    expected_sha256: String,
    replacements: Vec<TextReplacement>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateFile {
    path: String,
    content: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateDirectory {
    path: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, arguments: &str) -> ProviderToolCall {
        ProviderToolCall {
            provider_item_id: None,
            provider_call_id: "call_1".to_owned(),
            name: name.to_owned(),
            arguments: arguments.to_owned(),
        }
    }

    #[test]
    fn catalog_is_fixed_strict_and_complete() {
        let tools = provider_tools();
        assert_eq!(tools.len(), 6);
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            [
                "list_directory",
                "read_file",
                "search_text",
                "edit_file",
                "create_file",
                "create_directory"
            ]
        );
        assert!(
            tools
                .iter()
                .all(|tool| tool.parameters["additionalProperties"] == false)
        );
    }

    #[test]
    fn provider_calls_decode_into_closed_typed_inputs() {
        let parsed = parse_provider_calls(vec![call(
            "edit_file",
            r#"{"path":"src/lib.rs","expected_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","replacements":[{"old_text":"before","new_text":"after"}]}"#,
        )])
        .expect("valid call should decode");
        assert!(matches!(parsed[0].input, ToolInput::EditFile { .. }));

        for arguments in [
            r#"{"path":"src/lib.rs","start_line":1,"line_count":10,"extra":true}"#,
            r#"{"path":"src/lib.rs","start_line":1}"#,
            r#"{"path":"src/lib.rs","path":"other","start_line":1,"line_count":10}"#,
        ] {
            assert!(parse_provider_calls(vec![call("read_file", arguments)]).is_err());
        }
        assert!(parse_provider_calls(vec![call("unknown", r#"{"path":"."}"#)]).is_err());
    }
}
