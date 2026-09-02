mod catalog;
mod path;
mod worktree;

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) use catalog::{
    LEGACY_SANDBOX_TOOL_CATALOG_VERSION, TOOL_CATALOG_VERSION, ToolCallValidationError,
    developer_instruction, parse_provider_calls, provider_tools, validate_canonical_input,
};
pub(crate) use path::WorktreePath;
pub(crate) use worktree::{ToolExecution, WorktreeToolExecutor, recovery_plan_is_valid};

pub(crate) const TOOL_LIMITS_VERSION: u16 = 1;
pub(crate) const LEGACY_SANDBOX_TOOL_LIMITS_VERSION: u16 = 2;
pub(crate) const MAX_COMMAND_ARGUMENTS: usize = 128;
pub(crate) const MAX_COMMAND_ARGUMENT_BYTES: usize = 4096;
pub(crate) const MAX_COMMAND_ARGUMENT_TOTAL_BYTES: usize = 64 * 1024;
pub(crate) const MAX_COMMAND_OUTPUT_BYTES: usize = 256 * 1024;
pub(crate) const MAX_TOOL_CALLS_PER_TURN: usize = 8;
pub(crate) const MAX_TOOL_CALLS_PER_RUN: u32 = 64;
pub(crate) const MAX_TOOL_MUTATIONS_PER_RUN: u32 = 16;
pub(crate) const MAX_PROVIDER_TURNS_PER_RUN: u16 = 32;
pub(crate) const MAX_TOOL_RESULT_BYTES_PER_RUN: u64 = 2 * 1024 * 1024;
pub(crate) const MAX_TOOL_PAYLOAD_BYTES: usize = 512 * 1024;
pub(crate) const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
pub(crate) const MAX_READ_OUTPUT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_READ_LINES: u16 = 200;
pub(crate) const MAX_DIRECTORY_ENTRIES: usize = 200;
pub(crate) const MAX_SEARCH_QUERY_BYTES: usize = 512;
pub(crate) const MAX_SEARCH_FILES: u32 = 4_096;
pub(crate) const MAX_SEARCH_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) const MAX_SEARCH_MATCHES: usize = 200;
pub(crate) const MAX_SEARCH_OUTPUT_BYTES: usize = 128 * 1024;
pub(crate) const MAX_REPLACEMENTS: usize = 32;
pub(crate) const MAX_REPLACEMENT_BYTES: usize = 256 * 1024;
const TOOL_PATH_DIGEST_CONTEXT: &[u8] = b"morons.dev/tool-path/v1\0";

pub(crate) fn tool_path_digest(path: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(TOOL_PATH_DIGEST_CONTEXT);
    digest.update((path.len() as u32).to_be_bytes());
    digest.update(path.as_bytes());
    digest.finalize().into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolKind {
    ListDirectory,
    ReadFile,
    SearchText,
    EditFile,
    CreateFile,
    CreateDirectory,
    // Retained only to decode durable transcripts created before ADR 0012.
    RunCommand,
}

impl ToolKind {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::ListDirectory => "list_directory",
            Self::ReadFile => "read_file",
            Self::SearchText => "search_text",
            Self::EditFile => "edit_file",
            Self::CreateFile => "create_file",
            Self::CreateDirectory => "create_directory",
            Self::RunCommand => "run_command",
        }
    }

    pub(crate) const fn to_record(self) -> i64 {
        match self {
            Self::ListDirectory => 1,
            Self::ReadFile => 2,
            Self::SearchText => 3,
            Self::EditFile => 4,
            Self::CreateFile => 5,
            Self::CreateDirectory => 6,
            Self::RunCommand => 7,
        }
    }

    pub(crate) const fn from_record(value: i64) -> Option<Self> {
        match value {
            1 => Some(Self::ListDirectory),
            2 => Some(Self::ReadFile),
            3 => Some(Self::SearchText),
            4 => Some(Self::EditFile),
            5 => Some(Self::CreateFile),
            6 => Some(Self::CreateDirectory),
            7 => Some(Self::RunCommand),
            _ => None,
        }
    }

    pub(crate) const fn is_mutation(self) -> bool {
        matches!(
            self,
            Self::EditFile | Self::CreateFile | Self::CreateDirectory | Self::RunCommand
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ToolInput {
    ListDirectory {
        path: WorktreePath,
        after: Option<String>,
    },
    ReadFile {
        path: WorktreePath,
        start_line: u32,
        line_count: u16,
    },
    SearchText {
        path: WorktreePath,
        query: String,
    },
    EditFile {
        path: WorktreePath,
        expected_sha256: String,
        replacements: Vec<TextReplacement>,
    },
    CreateFile {
        path: WorktreePath,
        content: String,
    },
    CreateDirectory {
        path: WorktreePath,
    },
    RunCommand {
        executable: String,
        arguments: Vec<String>,
        working_directory: WorktreePath,
    },
}

impl ToolInput {
    pub(crate) const fn kind(&self) -> ToolKind {
        match self {
            Self::ListDirectory { .. } => ToolKind::ListDirectory,
            Self::ReadFile { .. } => ToolKind::ReadFile,
            Self::SearchText { .. } => ToolKind::SearchText,
            Self::EditFile { .. } => ToolKind::EditFile,
            Self::CreateFile { .. } => ToolKind::CreateFile,
            Self::CreateDirectory { .. } => ToolKind::CreateDirectory,
            Self::RunCommand { .. } => ToolKind::RunCommand,
        }
    }

    pub(crate) const fn path(&self) -> &WorktreePath {
        match self {
            Self::ListDirectory { path, .. }
            | Self::ReadFile { path, .. }
            | Self::SearchText { path, .. }
            | Self::EditFile { path, .. }
            | Self::CreateFile { path, .. }
            | Self::CreateDirectory { path }
            | Self::RunCommand {
                working_directory: path,
                ..
            } => path,
        }
    }

    pub(crate) fn provider_arguments(&self) -> Result<String, serde_json::Error> {
        match self {
            Self::ListDirectory { path, after } => serde_json::to_string(&ListDirectoryArguments {
                path: path.as_str(),
                after: after.as_deref(),
            }),
            Self::ReadFile {
                path,
                start_line,
                line_count,
            } => serde_json::to_string(&ReadFileArguments {
                path: path.as_str(),
                start_line: *start_line,
                line_count: *line_count,
            }),
            Self::SearchText { path, query } => serde_json::to_string(&SearchTextArguments {
                path: path.as_str(),
                query,
            }),
            Self::EditFile {
                path,
                expected_sha256,
                replacements,
            } => serde_json::to_string(&EditFileArguments {
                path: path.as_str(),
                expected_sha256,
                replacements,
            }),
            Self::CreateFile { path, content } => serde_json::to_string(&CreateFileArguments {
                path: path.as_str(),
                content,
            }),
            Self::CreateDirectory { path } => serde_json::to_string(&CreateDirectoryArguments {
                path: path.as_str(),
            }),
            Self::RunCommand {
                executable,
                arguments,
                working_directory,
            } => serde_json::to_string(&RunCommandArguments {
                executable,
                arguments,
                working_directory: working_directory.as_str(),
            }),
        }
    }
}

impl fmt::Display for ToolKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

#[derive(Serialize)]
struct RunCommandArguments<'a> {
    executable: &'a str,
    arguments: &'a [String],
    working_directory: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TextReplacement {
    pub old_text: String,
    pub new_text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ValidatedProviderCall {
    pub provider_call_id: String,
    pub input: ToolInput,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolErrorKind {
    InvalidPath,
    NotFound,
    WrongNodeKind,
    LinkOrReparsePoint,
    ChangedDuringOperation,
    BinaryFile,
    InvalidUtf8,
    DigestMismatch,
    ReplacementNotFound,
    ReplacementAmbiguous,
    ReplacementOverlap,
    AlreadyExists,
    ResourceLimit,
    Cancelled,
    Interrupted,
    NotDispatched,
    Uncertain,
    Filesystem,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ToolResult {
    Ok { output: ToolOutput },
    Error { error: ToolErrorKind },
}

impl ToolResult {
    pub(crate) const fn error(error: ToolErrorKind) -> Self {
        Self::Error { error }
    }

    pub(crate) const fn is_uncertain(&self) -> bool {
        matches!(
            self,
            Self::Error {
                error: ToolErrorKind::Uncertain
            }
        )
    }

    pub(crate) fn provider_output(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub(crate) fn summary(&self) -> String {
        match self {
            Self::Ok { output } => output.summary(),
            Self::Error { error } => format!("{} failed: {}", error.tool_label(), error.label()),
        }
    }
}

impl ToolErrorKind {
    const fn tool_label(self) -> &'static str {
        "tool"
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::InvalidPath => "invalid path",
            Self::NotFound => "not found",
            Self::WrongNodeKind => "wrong node kind",
            Self::LinkOrReparsePoint => "link or reparse point rejected",
            Self::ChangedDuringOperation => "node changed during operation",
            Self::BinaryFile => "binary file rejected",
            Self::InvalidUtf8 => "invalid UTF-8",
            Self::DigestMismatch => "file digest changed",
            Self::ReplacementNotFound => "replacement source not found",
            Self::ReplacementAmbiguous => "replacement source is ambiguous",
            Self::ReplacementOverlap => "replacement ranges overlap",
            Self::AlreadyExists => "target already exists",
            Self::ResourceLimit => "resource limit reached",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::NotDispatched => "not dispatched",
            Self::Uncertain => "workspace effect is uncertain",
            Self::Filesystem => "filesystem operation failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "output", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ToolOutput {
    DirectoryPage {
        path: WorktreePath,
        entries: Vec<DirectoryListEntry>,
        has_more: bool,
    },
    FileRead {
        path: WorktreePath,
        start_line: u32,
        end_line: u32,
        end_of_file: bool,
        sha256: String,
        text: String,
    },
    SearchMatches {
        path: WorktreePath,
        matches: Vec<SearchMatch>,
        skipped_binary_files: u32,
        truncated: bool,
    },
    FileEdited {
        path: WorktreePath,
        sha256: String,
        bytes: u64,
    },
    FileCreated {
        path: WorktreePath,
        sha256: String,
        bytes: u64,
    },
    DirectoryCreated {
        path: WorktreePath,
    },
    CommandCompleted {
        executable: String,
        exit_code: i32,
        stdout: String,
        stderr: String,
        published: bool,
    },
}

impl ToolOutput {
    pub(crate) const fn kind(&self) -> ToolKind {
        match self {
            Self::DirectoryPage { .. } => ToolKind::ListDirectory,
            Self::FileRead { .. } => ToolKind::ReadFile,
            Self::SearchMatches { .. } => ToolKind::SearchText,
            Self::FileEdited { .. } => ToolKind::EditFile,
            Self::FileCreated { .. } => ToolKind::CreateFile,
            Self::DirectoryCreated { .. } => ToolKind::CreateDirectory,
            Self::CommandCompleted { .. } => ToolKind::RunCommand,
        }
    }

    fn summary(&self) -> String {
        match self {
            Self::DirectoryPage {
                entries, has_more, ..
            } => format!(
                "listed {} entr{}{}",
                entries.len(),
                if entries.len() == 1 { "y" } else { "ies" },
                if *has_more { " (more available)" } else { "" }
            ),
            Self::FileRead {
                start_line,
                end_line,
                end_of_file,
                ..
            } => format!(
                "read lines {start_line}-{end_line}{}",
                if *end_of_file { " (end of file)" } else { "" }
            ),
            Self::SearchMatches {
                matches,
                skipped_binary_files,
                truncated,
                ..
            } => format!(
                "found {} match{}; skipped {} binary file{}{}",
                matches.len(),
                if matches.len() == 1 { "" } else { "es" },
                skipped_binary_files,
                if *skipped_binary_files == 1 { "" } else { "s" },
                if *truncated { " (truncated)" } else { "" }
            ),
            Self::FileEdited { bytes, .. } => format!("edited file ({bytes} bytes)"),
            Self::FileCreated { bytes, .. } => format!("created file ({bytes} bytes)"),
            Self::DirectoryCreated { .. } => "created directory".to_owned(),
            Self::CommandCompleted {
                executable,
                exit_code,
                stdout,
                stderr,
                published,
            } => {
                let mut summary = format!(
                    "ran {executable} with exit code {exit_code}{}",
                    if *published {
                        " and published changes"
                    } else {
                        ""
                    }
                );
                for (label, output) in [("stdout", stdout), ("stderr", stderr)] {
                    if !output.is_empty() {
                        summary.push('\n');
                        summary.push_str(label);
                        summary.push_str(":\n");
                        summary.push_str(bounded_text(output, 4_096));
                    }
                }
                summary
            }
        }
    }
}

fn bounded_text(value: &str, maximum: usize) -> &str {
    if value.len() <= maximum {
        return value;
    }
    let mut boundary = maximum;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    &value[..boundary]
}

pub(crate) fn validate_canonical_result(tool: ToolKind, result: &ToolResult) -> bool {
    match result {
        ToolResult::Error { .. } => true,
        ToolResult::Ok { output } if output.kind() != tool => false,
        ToolResult::Ok {
            output: ToolOutput::DirectoryPage { path, entries, .. },
        } => {
            WorktreePath::parse(path.as_str(), true).is_ok()
                && entries.len() <= MAX_DIRECTORY_ENTRIES
                && entries.iter().all(|entry| {
                    WorktreePath::parse(&entry.name, false)
                        .is_ok_and(|path| path.components().count() == 1)
                })
                && entries
                    .windows(2)
                    .all(|pair| pair[0].name.as_bytes() < pair[1].name.as_bytes())
        }
        ToolResult::Ok {
            output:
                ToolOutput::FileRead {
                    path,
                    start_line,
                    end_line,
                    sha256,
                    text,
                    ..
                },
        } => {
            WorktreePath::parse(path.as_str(), false).is_ok()
                && *start_line > 0
                && *end_line >= start_line.saturating_sub(1)
                && end_line.saturating_sub(*start_line).saturating_add(1)
                    <= u32::from(MAX_READ_LINES)
                && valid_digest(sha256)
                && text.len() <= MAX_READ_OUTPUT_BYTES
        }
        ToolResult::Ok {
            output: ToolOutput::SearchMatches { path, matches, .. },
        } => {
            WorktreePath::parse(path.as_str(), true).is_ok()
                && matches.len() <= MAX_SEARCH_MATCHES
                && matches.iter().all(|matched| {
                    matched.line > 0
                        && matched.text.len() <= 1_024
                        && WorktreePath::parse(matched.path.as_str(), false).is_ok()
                })
                && matches
                    .iter()
                    .try_fold(0_usize, |bytes, matched| {
                        bytes
                            .checked_add(matched.path.as_str().len())?
                            .checked_add(matched.text.len())
                    })
                    .is_some_and(|bytes| bytes <= MAX_SEARCH_OUTPUT_BYTES)
        }
        ToolResult::Ok {
            output:
                ToolOutput::FileEdited {
                    path,
                    sha256,
                    bytes,
                }
                | ToolOutput::FileCreated {
                    path,
                    sha256,
                    bytes,
                },
        } => {
            WorktreePath::parse(path.as_str(), false).is_ok()
                && valid_digest(sha256)
                && *bytes <= MAX_FILE_BYTES
        }
        ToolResult::Ok {
            output: ToolOutput::DirectoryCreated { path },
        } => WorktreePath::parse(path.as_str(), false).is_ok(),
        ToolResult::Ok {
            output:
                ToolOutput::CommandCompleted {
                    executable,
                    stdout,
                    stderr,
                    ..
                },
        } => {
            valid_command_executable(executable)
                && stdout.len() <= MAX_COMMAND_OUTPUT_BYTES
                && stderr.len() <= MAX_COMMAND_OUTPUT_BYTES
        }
    }
}

pub(crate) fn valid_command_executable(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectoryListEntry {
    pub name: String,
    pub kind: DirectoryEntryKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DirectoryEntryKind {
    File,
    Directory,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SearchMatch {
    pub path: WorktreePath,
    pub line: u32,
    pub text: String,
}

#[derive(Serialize)]
struct ListDirectoryArguments<'a> {
    path: &'a str,
    after: Option<&'a str>,
}

#[derive(Serialize)]
struct ReadFileArguments<'a> {
    path: &'a str,
    start_line: u32,
    line_count: u16,
}

#[derive(Serialize)]
struct SearchTextArguments<'a> {
    path: &'a str,
    query: &'a str,
}

#[derive(Serialize)]
struct EditFileArguments<'a> {
    path: &'a str,
    expected_sha256: &'a str,
    replacements: &'a [TextReplacement],
}

#[derive(Serialize)]
struct CreateFileArguments<'a> {
    path: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct CreateDirectoryArguments<'a> {
    path: &'a str,
}
