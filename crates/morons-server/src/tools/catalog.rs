use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    MAX_BASH_COMMAND_BYTES, MAX_COMMAND_ARGUMENTS, MAX_FILE_BYTES, MAX_READ_LINES,
    MAX_REPLACEMENT_BYTES, MAX_REPLACEMENTS, MAX_SEARCH_QUERY_BYTES, MAX_TOOL_CALLS_PER_TURN,
    TextReplacement, ToolInput, ToolKind, ToolPath, ValidatedProviderCall, WorktreePath,
};
use crate::provider::{ProviderTool, ProviderToolCall, json::parse_strict_value};

pub(crate) const TOOL_CATALOG_VERSION: u16 = 4;
pub(crate) const LEGACY_SANDBOX_TOOL_CATALOG_VERSION: u16 = 2;

const DEVELOPER_INSTRUCTION: &str = "You are operating directly in the user's selected working directory with the user's normal local-user authority. Relative paths resolve from that directory; absolute paths and normal operating-system path semantics are allowed. Use read, write, and edit for bounded file operations and bash for bounded noninteractive Bash commands. Edit requires exact unique non-overlapping replacements. Bash has closed stdin and no PTY; it inherits the user's ordinary development environment and may access the network and user credentials. These tools are not sandboxed, and cancellation cannot undo completed effects. Never assume that a tool succeeded until its committed result says so.";

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
            name: ToolKind::Read.name().to_owned(),
            description: "Read a bounded UTF-8 line window from one file. Relative paths resolve from the selected working directory; absolute paths are allowed.".to_owned(),
            parameters: object_schema(
                json!({
                    "path": {"type": "string", "maxLength": super::path::MAX_TOOL_PATH_BYTES},
                    "offset": {"type": "integer", "minimum": 1, "maximum": 4294967295_u64},
                    "limit": {"type": "integer", "minimum": 1, "maximum": MAX_READ_LINES}
                }),
                &["path", "offset", "limit"],
            ),
        },
        ProviderTool {
            name: ToolKind::Write.name().to_owned(),
            description: "Write one complete bounded UTF-8 file, creating or replacing it with normal filesystem semantics.".to_owned(),
            parameters: object_schema(
                json!({
                    "path": {"type": "string", "maxLength": super::path::MAX_TOOL_PATH_BYTES},
                    "content": {"type": "string", "maxLength": MAX_FILE_BYTES}
                }),
                &["path", "content"],
            ),
        },
        ProviderTool {
            name: ToolKind::Edit.name().to_owned(),
            description: "Apply exact unique non-overlapping replacements to one bounded UTF-8 file. Every replacement must match exactly once.".to_owned(),
            parameters: object_schema(
                json!({
                    "path": {"type": "string", "maxLength": super::path::MAX_TOOL_PATH_BYTES},
                    "replacements": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_REPLACEMENTS,
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "old_text": {"type": "string", "minLength": 1, "maxLength": MAX_REPLACEMENT_BYTES},
                                "new_text": {"type": "string", "maxLength": MAX_REPLACEMENT_BYTES}
                            },
                            "required": ["old_text", "new_text"]
                        }
                    }
                }),
                &["path", "replacements"],
            ),
        },
        ProviderTool {
            name: ToolKind::Bash.name().to_owned(),
            description: "Run one bounded noninteractive Bash command in the selected working directory with the user's normal development environment. Standard input is closed; stdout and stderr are captured separately. This is not sandboxed.".to_owned(),
            parameters: object_schema(
                json!({
                    "command": {"type": "string", "minLength": 1, "maxLength": MAX_BASH_COMMAND_BYTES}
                }),
                &["command"],
            ),
        },
    ]
}

pub(crate) fn validate_canonical_input(input: &ToolInput) -> bool {
    input
        .provider_arguments()
        .ok()
        .and_then(|arguments| parse_strict_value(arguments.as_bytes()).ok())
        .and_then(|value| parse_input(input.kind().name(), value, true).ok())
        .as_ref()
        == Some(input)
}

pub(crate) fn parse_provider_calls(
    calls: Vec<ProviderToolCall>,
    catalog_version: u16,
) -> Result<Vec<ValidatedProviderCall>, ToolCallValidationError> {
    if calls.is_empty() || catalog_version != TOOL_CATALOG_VERSION {
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
            let input = parse_input(&call.name, value, false)?;
            Ok(ValidatedProviderCall {
                provider_call_id: call.provider_call_id,
                input,
            })
        })
        .collect()
}

fn parse_input(
    name: &str,
    value: Value,
    allow_legacy_command: bool,
) -> Result<ToolInput, ToolCallValidationError> {
    match name {
        "read" => {
            require_fields(&value, &["path", "offset", "limit"])?;
            let arguments: Read = decode(value)?;
            if arguments.offset == 0 || arguments.limit == 0 || arguments.limit > MAX_READ_LINES {
                return Err(ToolCallValidationError::InvalidProviderOutput);
            }
            Ok(ToolInput::Read {
                path: ToolPath::parse(&arguments.path).map_err(invalid)?,
                offset: arguments.offset,
                limit: arguments.limit,
            })
        }
        "write" => {
            require_fields(&value, &["path", "content"])?;
            let arguments: Write = decode(value)?;
            if arguments.content.len() as u64 > MAX_FILE_BYTES {
                return Err(ToolCallValidationError::ResourceLimit);
            }
            Ok(ToolInput::Write {
                path: ToolPath::parse(&arguments.path).map_err(invalid)?,
                content: arguments.content,
            })
        }
        "edit" => {
            require_fields(&value, &["path", "replacements"])?;
            let arguments: Edit = decode(value)?;
            if arguments.replacements.is_empty()
                || arguments.replacements.len() > MAX_REPLACEMENTS
                || arguments
                    .replacements
                    .iter()
                    .any(|replacement| replacement.old_text.is_empty())
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
            Ok(ToolInput::Edit {
                path: ToolPath::parse(&arguments.path).map_err(invalid)?,
                replacements: arguments.replacements,
            })
        }
        "bash" => {
            require_fields(&value, &["command"])?;
            let arguments: Bash = decode(value)?;
            if arguments.command.is_empty() || arguments.command.contains('\0') {
                return Err(ToolCallValidationError::InvalidProviderOutput);
            }
            if arguments.command.len() > MAX_BASH_COMMAND_BYTES {
                return Err(ToolCallValidationError::ResourceLimit);
            }
            Ok(ToolInput::Bash {
                command: arguments.command,
            })
        }
        "list_directory" if allow_legacy_command => {
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
        "read_file" if allow_legacy_command => {
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
        "search_text" if allow_legacy_command => {
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
        "edit_file" if allow_legacy_command => {
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
        "create_file" if allow_legacy_command => {
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
        "create_directory" if allow_legacy_command => {
            require_fields(&value, &["path"])?;
            let arguments: CreateDirectory = decode(value)?;
            Ok(ToolInput::CreateDirectory {
                path: WorktreePath::parse(&arguments.path, false).map_err(invalid)?,
            })
        }
        "run_command" if allow_legacy_command => {
            require_fields(&value, &["executable", "arguments", "working_directory"])?;
            let arguments: RunCommand = decode(value)?;
            let total = arguments
                .arguments
                .iter()
                .try_fold(0_usize, |total, argument| {
                    if argument.len() > super::MAX_COMMAND_ARGUMENT_BYTES || argument.contains('\0')
                    {
                        return None;
                    }
                    total.checked_add(argument.len())
                });
            if !super::valid_command_executable(&arguments.executable)
                || arguments.arguments.len() > MAX_COMMAND_ARGUMENTS
                || total.is_none_or(|bytes| bytes > super::MAX_COMMAND_ARGUMENT_TOTAL_BYTES)
            {
                return Err(ToolCallValidationError::ResourceLimit);
            }
            Ok(ToolInput::RunCommand {
                executable: arguments.executable,
                arguments: arguments.arguments,
                working_directory: WorktreePath::parse(&arguments.working_directory, true)
                    .map_err(invalid)?,
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
struct Read {
    path: String,
    offset: u32,
    limit: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Write {
    path: String,
    content: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    path: String,
    replacements: Vec<TextReplacement>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bash {
    command: String,
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunCommand {
    executable: String,
    arguments: Vec<String>,
    working_directory: String,
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
        assert_eq!(tools.len(), 4);
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["read", "write", "edit", "bash"]
        );
        assert!(
            tools
                .iter()
                .all(|tool| tool.parameters["additionalProperties"] == false)
        );
    }

    #[test]
    fn provider_calls_decode_into_closed_typed_inputs() {
        let parsed = parse_provider_calls(
            vec![call(
                "edit",
                r#"{"path":"src/lib.rs","replacements":[{"old_text":"before","new_text":"after"}]}"#,
            )],
            TOOL_CATALOG_VERSION,
        )
        .expect("valid call should decode");
        assert!(matches!(parsed[0].input, ToolInput::Edit { .. }));
        assert!(matches!(
            parse_provider_calls(
                vec![call("bash", r#"{"command":"cargo test --locked"}"#)],
                TOOL_CATALOG_VERSION,
            )
            .expect("valid bash should decode")[0]
                .input,
            ToolInput::Bash { .. }
        ));

        for arguments in [
            r#"{"path":"src/lib.rs","offset":1,"limit":10,"extra":true}"#,
            r#"{"path":"src/lib.rs","offset":1}"#,
            r#"{"path":"src/lib.rs","path":"other","offset":1,"limit":10}"#,
        ] {
            assert!(
                parse_provider_calls(vec![call("read", arguments)], TOOL_CATALOG_VERSION,).is_err()
            );
        }
        assert!(
            parse_provider_calls(
                vec![call("unknown", r#"{"path":"."}"#)],
                TOOL_CATALOG_VERSION,
            )
            .is_err()
        );
        let command = call(
            "run_command",
            r#"{"executable":"cargo","arguments":["check","--locked"],"working_directory":"."}"#,
        );
        assert!(parse_provider_calls(vec![command], TOOL_CATALOG_VERSION).is_err());
        assert!(validate_canonical_input(&ToolInput::RunCommand {
            executable: "cargo".to_owned(),
            arguments: vec!["check".to_owned(), "--locked".to_owned()],
            working_directory: WorktreePath::parse(".", true).expect("root path should parse"),
        }));
    }
}
