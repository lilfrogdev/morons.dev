mod bash;
mod catalog;
mod direct;
mod ipython;
mod path;
mod web_search;
mod worktree;

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) use bash::BashToolExecutor;
pub(crate) use catalog::{
    LEGACY_SANDBOX_TOOL_CATALOG_VERSION, TOOL_CATALOG_VERSION, ToolCallValidationError,
    developer_instruction, parse_provider_calls, parse_subagent_provider_calls, provider_tools,
    subagent_provider_tools, validate_canonical_input,
};
pub(crate) use direct::DirectToolExecutor;
pub(crate) use ipython::{IpythonSupervisor, validate_ipython_cell};
pub(crate) use path::{ToolPath, WorktreePath};
pub(crate) use web_search::WebSearchToolExecutor;
pub(crate) use worktree::recovery_plan_is_valid;

pub(crate) const TOOL_LIMITS_VERSION: u16 = 9;
pub(crate) const LEGACY_WORKTREE_TOOL_CATALOG_VERSION: u16 = 1;
pub(crate) const LEGACY_WORKTREE_TOOL_LIMITS_VERSION: u16 = 1;
pub(crate) const LEGACY_SANDBOX_TOOL_LIMITS_VERSION: u16 = 2;
pub(crate) const MAX_BASH_COMMAND_BYTES: usize = 64 * 1024;
pub(crate) const MAX_BASH_OUTPUT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_IPYTHON_CELL_BYTES: usize = 64 * 1024;
pub(crate) const MAX_IPYTHON_OUTPUT_BYTES: usize = 64 * 1024;
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
pub(crate) const MAX_SEARCH_MATCHES: usize = 200;
pub(crate) const MAX_SEARCH_OUTPUT_BYTES: usize = 128 * 1024;
pub(crate) const MAX_WEB_SEARCH_QUERY_BYTES: usize = 512;
pub(crate) const MAX_WEB_SEARCH_RESULTS: usize = 10;
pub(crate) const MAX_WEB_SEARCH_TITLE_BYTES: usize = 512;
pub(crate) const MAX_WEB_SEARCH_URL_BYTES: usize = 4 * 1024;
pub(crate) const MAX_WEB_SEARCH_SNIPPET_BYTES: usize = 4 * 1024;
pub(crate) const MAX_WEB_SEARCH_BODY_BYTES: usize = 512 * 1024;
pub(crate) const MAX_REPLACEMENTS: usize = 32;
pub(crate) const MAX_REPLACEMENT_BYTES: usize = 256 * 1024;
pub(crate) const MAX_SUBAGENT_TASKS: usize = 3;
pub(crate) const MAX_SUBAGENT_CONTEXT_BYTES: usize = 32 * 1024;
pub(crate) const MAX_SUBAGENT_ASSIGNMENT_BYTES: usize = 16 * 1024;
pub(crate) const MAX_SUBAGENT_NAME_BYTES: usize = 64;
pub(crate) const MAX_SUBAGENT_OUTPUT_BYTES: usize = 32 * 1024;
pub(crate) const MAX_SUBAGENT_PROVIDER_TURNS: u16 = 8;
pub(crate) const MAX_SUBAGENT_TOOL_CALLS: u16 = 24;
pub(crate) const MAX_SUBAGENT_MUTATIONS: u16 = 8;
pub(crate) const MAX_TASK_CALLS_PER_RUN: u32 = 2;
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
    Read,
    Write,
    Edit,
    Bash,
    WebSearch,
    Ipython,
    Task,
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
            Self::Read => "read",
            Self::Write => "write",
            Self::Edit => "edit",
            Self::Bash => "bash",
            Self::WebSearch => "web_search",
            Self::Ipython => "ipython",
            Self::Task => "task",
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
            Self::Read => 8,
            Self::Write => 9,
            Self::Edit => 10,
            Self::Bash => 11,
            Self::WebSearch => 12,
            Self::Ipython => 13,
            Self::Task => 14,
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
            8 => Some(Self::Read),
            9 => Some(Self::Write),
            10 => Some(Self::Edit),
            11 => Some(Self::Bash),
            12 => Some(Self::WebSearch),
            13 => Some(Self::Ipython),
            14 => Some(Self::Task),
            _ => None,
        }
    }

    pub(crate) const fn is_mutation(self) -> bool {
        matches!(
            self,
            Self::EditFile
                | Self::CreateFile
                | Self::CreateDirectory
                | Self::RunCommand
                | Self::Write
                | Self::Edit
                | Self::Bash
                | Self::Ipython
                | Self::Task
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
    Read {
        path: ToolPath,
        offset: u32,
        limit: u16,
    },
    Write {
        path: ToolPath,
        content: String,
    },
    Edit {
        path: ToolPath,
        replacements: Vec<TextReplacement>,
    },
    Bash {
        command: String,
    },
    WebSearch {
        query: String,
    },
    Ipython {
        cell: String,
    },
    Task {
        context: String,
        tasks: Vec<SubagentTask>,
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
            Self::Read { .. } => ToolKind::Read,
            Self::Write { .. } => ToolKind::Write,
            Self::Edit { .. } => ToolKind::Edit,
            Self::Bash { .. } => ToolKind::Bash,
            Self::WebSearch { .. } => ToolKind::WebSearch,
            Self::Ipython { .. } => ToolKind::Ipython,
            Self::Task { .. } => ToolKind::Task,
        }
    }

    pub(crate) fn path_text(&self) -> &str {
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
            } => path.as_str(),
            Self::Read { path, .. } | Self::Write { path, .. } | Self::Edit { path, .. } => {
                path.as_str()
            }
            Self::Bash { command } => command,
            Self::WebSearch { query } => query,
            Self::Ipython { cell } => cell,
            Self::Task { context, .. } => context,
        }
    }

    pub(crate) fn presentation_text(&self) -> String {
        match self {
            Self::Task { tasks, .. } => format!(
                "{} subagent task{}",
                tasks.len(),
                if tasks.len() == 1 { "" } else { "s" }
            ),
            _ => self.path_text().to_owned(),
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
            Self::Read {
                path,
                offset,
                limit,
            } => serde_json::to_string(&ReadArguments {
                path: path.as_str(),
                offset: *offset,
                limit: *limit,
            }),
            Self::Write { path, content } => serde_json::to_string(&WriteArguments {
                path: path.as_str(),
                content,
            }),
            Self::Edit { path, replacements } => serde_json::to_string(&EditArguments {
                path: path.as_str(),
                replacements,
            }),
            Self::Bash { command } => serde_json::to_string(&BashArguments { command }),
            Self::WebSearch { query } => serde_json::to_string(&WebSearchArguments { query }),
            Self::Ipython { cell } => serde_json::to_string(&IpythonArguments { cell }),
            Self::Task { context, tasks } => {
                serde_json::to_string(&TaskArguments { context, tasks })
            }
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

#[derive(Serialize)]
struct ReadArguments<'a> {
    path: &'a str,
    offset: u32,
    limit: u16,
}

#[derive(Serialize)]
struct WriteArguments<'a> {
    path: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct EditArguments<'a> {
    path: &'a str,
    replacements: &'a [TextReplacement],
}

#[derive(Serialize)]
struct BashArguments<'a> {
    command: &'a str,
}

#[derive(Serialize)]
struct WebSearchArguments<'a> {
    query: &'a str,
}

#[derive(Serialize)]
struct IpythonArguments<'a> {
    cell: &'a str,
}

#[derive(Serialize)]
struct TaskArguments<'a> {
    context: &'a str,
    tasks: &'a [SubagentTask],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubagentTask {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub task: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SubagentStatus {
    Succeeded,
    Failed,
    ResourceLimit,
}

impl SubagentStatus {
    const fn label(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::ResourceLimit => "resource limit",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubagentUsage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_write_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubagentModelDisclosure {
    pub service: String,
    pub model_id: String,
    pub protocol_revision: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubagentResult {
    pub index: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub status: SubagentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<SubagentModelDisclosure>,
    pub output: String,
    pub provider_turns: u16,
    pub tool_calls: u16,
    pub tool_mutations: u16,
    pub usage: SubagentUsage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TextReplacement {
    pub old_text: String,
    pub new_text: String,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ValidatedProviderCall {
    pub provider_call_id: String,
    pub input: ToolInput,
    pub opaque_continuation: Option<String>,
}

impl fmt::Debug for ValidatedProviderCall {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ValidatedProviderCall")
            .field("provider_call_id", &self.provider_call_id)
            .field("input", &self.input)
            .field(
                "opaque_continuation",
                &self.opaque_continuation.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
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
    OutputLimit,
    TimedOut,
    InactivityTimeout,
    Cancelled,
    Interrupted,
    NotDispatched,
    Uncertain,
    Filesystem,
    Network,
    InvalidResponse,
    CredentialNotConfigured,
    KernelUnavailable,
    ExecutionFailed,
    ImageInputUnsupported,
    ModelUnavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ToolResult {
    Ok {
        output: ToolOutput,
    },
    Error {
        error: ToolErrorKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<ToolOutput>,
    },
}

impl ToolResult {
    pub(crate) const fn error(error: ToolErrorKind) -> Self {
        Self::Error {
            error,
            output: None,
        }
    }

    pub(crate) const fn error_with_output(error: ToolErrorKind, output: ToolOutput) -> Self {
        Self::Error {
            error,
            output: Some(output),
        }
    }

    pub(crate) const fn error_kind(&self) -> Option<ToolErrorKind> {
        match self {
            Self::Ok { .. } => None,
            Self::Error { error, .. } => Some(*error),
        }
    }

    pub(crate) const fn has_image(&self) -> bool {
        matches!(
            self,
            Self::Ok {
                output: ToolOutput::ReadImage { .. }
            }
        )
    }

    pub(crate) const fn is_uncertain(&self) -> bool {
        matches!(self.error_kind(), Some(ToolErrorKind::Uncertain))
    }

    pub(crate) fn provider_output(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub(crate) fn summary(&self) -> String {
        match self {
            Self::Ok { output } => output.summary(),
            Self::Error { error, output } => {
                let mut summary = format!("{} failed: {}", error.tool_label(), error.label());
                if let Some(output) = output {
                    summary.push('\n');
                    summary.push_str(&output.summary());
                }
                summary
            }
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
            Self::OutputLimit => "output limit reached",
            Self::TimedOut => "wall-clock timeout reached",
            Self::InactivityTimeout => "inactivity timeout reached",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::NotDispatched => "not dispatched",
            Self::Uncertain => "local effect is uncertain",
            Self::Filesystem => "filesystem operation failed",
            Self::Network => "network request failed",
            Self::InvalidResponse => "search service returned an invalid response",
            Self::CredentialNotConfigured => "search credential is not configured",
            Self::KernelUnavailable => {
                "IPython runtime unavailable; reinstall the matching Morons package, retry first setup with network access, or set MORONS_PYTHON to an expert-managed Python with jupyter_client and ipykernel"
            }
            Self::ExecutionFailed => "IPython cell failed",
            Self::ImageInputUnsupported => "selected model does not support image input",
            Self::ModelUnavailable => "configured subagent model is unavailable",
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
    Read {
        path: ToolPath,
        offset: u32,
        next_offset: u32,
        end_of_file: bool,
        text: String,
    },
    ReadImage {
        path: ToolPath,
        image: ToolImageOutput,
    },
    Written {
        path: ToolPath,
        bytes: u64,
    },
    Edited {
        path: ToolPath,
        replacements: u16,
        bytes: u64,
    },
    Bash {
        exit_code: Option<i32>,
        signal: Option<u16>,
        stdout: String,
        stderr: String,
    },
    WebSearch {
        query: String,
        results: Vec<WebSearchResult>,
        truncated: bool,
    },
    Ipython {
        execution_count: Option<u32>,
        stdout: String,
        stderr: String,
        display: String,
    },
    Task {
        results: Vec<SubagentResult>,
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
            Self::Read { .. } | Self::ReadImage { .. } => ToolKind::Read,
            Self::Written { .. } => ToolKind::Write,
            Self::Edited { .. } => ToolKind::Edit,
            Self::Bash { .. } => ToolKind::Bash,
            Self::WebSearch { .. } => ToolKind::WebSearch,
            Self::Ipython { .. } => ToolKind::Ipython,
            Self::Task { .. } => ToolKind::Task,
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
            Self::ReadImage { image, .. } => format!(
                "read image [{}] ({}×{}, {}, {} bytes)",
                image.display_name,
                image.width,
                image.height,
                image.media_type.as_str(),
                image.bytes
            ),
            Self::Read {
                offset,
                next_offset,
                end_of_file,
                ..
            } => format!(
                "read lines {offset}-{}{}",
                next_offset.saturating_sub(1),
                if *end_of_file { " (end of file)" } else { "" }
            ),
            Self::Written { bytes, .. } => format!("wrote file ({bytes} bytes)"),
            Self::Edited {
                replacements,
                bytes,
                ..
            } => format!("applied {replacements} replacement(s) ({bytes} bytes)"),
            Self::Ipython {
                execution_count,
                stdout,
                stderr,
                display,
            } => {
                let mut summary = execution_count.map_or_else(
                    || "IPython cell completed".to_owned(),
                    |count| format!("IPython cell {count} completed"),
                );
                for (label, output) in [("stdout", stdout), ("stderr", stderr), ("result", display)]
                {
                    if !output.is_empty() {
                        summary.push('\n');
                        summary.push_str(label);
                        summary.push_str(":\n");
                        summary.push_str(output);
                    }
                }
                summary
            }
            Self::WebSearch {
                results, truncated, ..
            } => format!(
                "found {} cited web result{}{}",
                results.len(),
                if results.len() == 1 { "" } else { "s" },
                if *truncated { " (truncated)" } else { "" }
            ),
            Self::Bash {
                exit_code,
                signal,
                stdout,
                stderr,
            } => {
                let mut summary = match (exit_code, signal) {
                    (Some(code), None) => format!("bash exited with code {code}"),
                    (None, Some(signal)) => format!("bash exited from signal {signal}"),
                    _ => "bash exit status unavailable".to_owned(),
                };
                for (label, output) in [("stdout", stdout), ("stderr", stderr)] {
                    if !output.is_empty() {
                        summary.push('\n');
                        summary.push_str(label);
                        summary.push_str(":\n");
                        summary.push_str(output);
                    }
                }
                summary
            }
            Self::Task { results } => {
                let succeeded = results
                    .iter()
                    .filter(|result| result.status == SubagentStatus::Succeeded)
                    .count();
                let mut summary = format!(
                    "completed {} subagent{} ({succeeded} succeeded)",
                    results.len(),
                    if results.len() == 1 { "" } else { "s" }
                );
                for result in results {
                    summary.push_str("\n\nsubagent ");
                    summary.push_str(&result.index.to_string());
                    if let Some(name) = &result.name {
                        summary.push_str(" (");
                        summary.push_str(name);
                        summary.push(')');
                    }
                    summary.push_str(&format!(
                        ": {}; {} provider turn(s), {} tool call(s), {} total token(s)",
                        result.status.label(),
                        result.provider_turns,
                        result.tool_calls,
                        result.usage.total_tokens
                    ));
                    if let Some(model) = &result.model {
                        summary.push_str(&format!(
                            "; {} / {} · protocol revision {}",
                            model.service, model.model_id, model.protocol_revision
                        ));
                    }
                    summary.push('\n');
                    summary.push_str(&result.output);
                }
                summary
            }
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
        ToolResult::Error { error, output } => output.as_ref().is_none_or(|output| match tool {
            ToolKind::Bash => {
                matches!(
                    error,
                    ToolErrorKind::OutputLimit
                        | ToolErrorKind::TimedOut
                        | ToolErrorKind::InactivityTimeout
                        | ToolErrorKind::Cancelled
                        | ToolErrorKind::Uncertain
                ) && output.kind() == ToolKind::Bash
                    && validate_bash_output(output, false)
            }
            ToolKind::Ipython => {
                matches!(
                    error,
                    ToolErrorKind::ExecutionFailed
                        | ToolErrorKind::OutputLimit
                        | ToolErrorKind::TimedOut
                        | ToolErrorKind::InactivityTimeout
                        | ToolErrorKind::Cancelled
                        | ToolErrorKind::Interrupted
                        | ToolErrorKind::Uncertain
                ) && output.kind() == ToolKind::Ipython
                    && validate_ipython_output(output)
            }
            _ => false,
        }),
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
                ToolOutput::Read {
                    path,
                    offset,
                    next_offset,
                    text,
                    ..
                },
        } => {
            ToolPath::parse(path.as_str()).is_ok()
                && *offset > 0
                && *next_offset >= *offset
                && next_offset.saturating_sub(*offset) <= u32::from(MAX_READ_LINES)
                && text.len() <= MAX_READ_OUTPUT_BYTES
        }
        ToolResult::Ok {
            output: ToolOutput::ReadImage { path, image },
        } => ToolPath::parse(path.as_str()).is_ok() && image.canonical_is_valid(),
        ToolResult::Ok {
            output: ToolOutput::Written { path, bytes },
        } => ToolPath::parse(path.as_str()).is_ok() && *bytes <= MAX_FILE_BYTES,
        ToolResult::Ok {
            output:
                ToolOutput::Edited {
                    path,
                    replacements,
                    bytes,
                },
        } => {
            ToolPath::parse(path.as_str()).is_ok()
                && *replacements > 0
                && usize::from(*replacements) <= MAX_REPLACEMENTS
                && *bytes <= MAX_FILE_BYTES
        }
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
        ToolResult::Ok {
            output: ToolOutput::WebSearch { query, results, .. },
        } => {
            valid_web_search_query(query)
                && results.len() <= MAX_WEB_SEARCH_RESULTS
                && results.iter().all(WebSearchResult::is_valid)
        }
        ToolResult::Ok {
            output: output @ ToolOutput::Ipython { .. },
        } => validate_ipython_output(output),
        ToolResult::Ok {
            output: ToolOutput::Task { results },
        } => validate_subagent_results(results),
        ToolResult::Ok { output } => validate_bash_output(output, true),
    }
}

pub(crate) fn valid_subagent_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SUBAGENT_NAME_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn validate_subagent_results(results: &[SubagentResult]) -> bool {
    !results.is_empty()
        && results.len() <= MAX_SUBAGENT_TASKS
        && results.iter().enumerate().all(|(index, result)| {
            result.index == u16::try_from(index + 1).unwrap_or(u16::MAX)
                && result.name.as_deref().is_none_or(valid_subagent_name)
                && result.model.as_ref().is_none_or(|model| {
                    matches!(model.service.as_str(), "OpenCode Zen" | "OpenCode Go")
                        && !model.model_id.is_empty()
                        && model.model_id.len() <= 128
                        && model.model_id.bytes().all(|byte| {
                            byte.is_ascii_lowercase()
                                || byte.is_ascii_digit()
                                || matches!(byte, b'.' | b'-' | b'_')
                        })
                        && model.protocol_revision > 0
                })
                && result.output.len() <= MAX_SUBAGENT_OUTPUT_BYTES
                && result.provider_turns <= MAX_SUBAGENT_PROVIDER_TURNS
                && result.tool_calls <= MAX_SUBAGENT_TOOL_CALLS
                && result.tool_mutations <= MAX_SUBAGENT_MUTATIONS
        })
}

pub(crate) fn validate_canonical_result_for_input(input: &ToolInput, result: &ToolResult) -> bool {
    if !validate_canonical_result(input.kind(), result) {
        return false;
    }
    match (input, result) {
        (
            ToolInput::Task { tasks, .. },
            ToolResult::Ok {
                output: ToolOutput::Task { results },
            },
        ) => {
            tasks.len() == results.len()
                && tasks
                    .iter()
                    .zip(results)
                    .all(|(task, result)| task.name == result.name)
        }
        (ToolInput::Task { .. }, ToolResult::Ok { .. }) => false,
        _ => true,
    }
}

fn validate_ipython_output(output: &ToolOutput) -> bool {
    match output {
        ToolOutput::Ipython {
            stdout,
            stderr,
            display,
            ..
        } => stdout
            .len()
            .checked_add(stderr.len())
            .and_then(|bytes| bytes.checked_add(display.len()))
            .is_some_and(|bytes| bytes <= MAX_IPYTHON_OUTPUT_BYTES),
        _ => false,
    }
}

fn validate_bash_output(output: &ToolOutput, require_exit: bool) -> bool {
    match output {
        ToolOutput::Bash {
            exit_code,
            signal,
            stdout,
            stderr,
        } => {
            ((!require_exit && exit_code.is_none() && signal.is_none())
                || (exit_code.is_some() ^ signal.is_some()))
                && stdout.len() <= MAX_BASH_OUTPUT_BYTES
                && stderr.len() <= MAX_BASH_OUTPUT_BYTES
        }
        _ => false,
    }
}

pub(crate) fn valid_command_executable(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

fn valid_web_search_query(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_WEB_SEARCH_QUERY_BYTES
        && !value.contains(['\0', '\r', '\n'])
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

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolImageOutput {
    pub attachment_id: Option<[u8; 16]>,
    pub display_name: String,
    pub media_type: morons_image::ImageMediaType,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data: Vec<u8>,
}

impl fmt::Debug for ToolImageOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolImageOutput")
            .field("attachment_id", &self.attachment_id)
            .field("display_name_bytes", &self.display_name.len())
            .field("media_type", &self.media_type)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.bytes)
            .field("sha256", &"[REDACTED]")
            .field("data", &"[REDACTED]")
            .finish()
    }
}

impl ToolImageOutput {
    fn canonical_is_valid(&self) -> bool {
        self.attachment_id
            .is_some_and(|id| id.iter().any(|byte| *byte != 0))
            && crate::persistence::images::valid_display_name(&self.display_name)
            && self.width > 0
            && self.height > 0
            && self.width <= morons_image::MAX_IMAGE_DIMENSION
            && self.height <= morons_image::MAX_IMAGE_DIMENSION
            && self.bytes > 0
            && self.bytes <= morons_image::MAX_NORMALIZED_IMAGE_BYTES as u64
            && valid_digest(&self.sha256)
            && self.data.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WebSearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

impl WebSearchResult {
    fn is_valid(&self) -> bool {
        self.title.len() <= MAX_WEB_SEARCH_TITLE_BYTES
            && self.url.len() <= MAX_WEB_SEARCH_URL_BYTES
            && self.snippet.len() <= MAX_WEB_SEARCH_SNIPPET_BYTES
            && !self.url.is_empty()
            && self.url.is_ascii()
            && !self
                .url
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
            && self.url.parse::<http::Uri>().is_ok_and(|uri| {
                matches!(uri.scheme_str(), Some("http" | "https")) && uri.authority().is_some()
            })
    }
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
