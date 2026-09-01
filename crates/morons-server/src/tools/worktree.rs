#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use std::{
    fmt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    DirectoryEntryKind, DirectoryListEntry, MAX_DIRECTORY_ENTRIES, MAX_FILE_BYTES,
    MAX_READ_OUTPUT_BYTES, MAX_REPLACEMENT_BYTES, MAX_REPLACEMENTS, SearchMatch, TextReplacement,
    ToolErrorKind, ToolInput, ToolOutput, ToolResult, WorktreePath,
};

#[cfg(unix)]
use self::unix as platform;
#[cfg(windows)]
use self::windows as platform;

const RECOVERY_PLAN_VERSION: u16 = 1;
const TOOL_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) struct WorktreeToolExecutor {
    root: PathBuf,
}

pub(crate) enum ToolExecution {
    Observation(ToolInput),
    Mutation(Box<PreparedMutation>),
    Complete(ToolResult),
}

pub(crate) struct PreparedMutation {
    plan: RecoveryPlan,
    inner: platform::PreparedMutation,
}

impl fmt::Debug for PreparedMutation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedMutation")
            .field("kind", &self.plan.kind)
            .field("path", &self.plan.path)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolRecoveryOutcome {
    Completed,
    NotApplied,
    Uncertain,
}

impl WorktreeToolExecutor {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(crate) fn prepare<F>(
        &self,
        input: ToolInput,
        operation_id: [u8; 16],
        cancelled: &F,
    ) -> ToolExecution
    where
        F: Fn() -> bool,
    {
        let deadline = Instant::now() + TOOL_OPERATION_TIMEOUT;
        let stopped = || cancelled() || Instant::now() >= deadline;
        if stopped() {
            return ToolExecution::Complete(ToolResult::error(ToolErrorKind::Cancelled));
        }
        match input {
            ToolInput::EditFile {
                path,
                expected_sha256,
                replacements,
            } => match platform::prepare_edit(
                &self.root,
                &path,
                &expected_sha256,
                &replacements,
                operation_id,
                &stopped,
            ) {
                Ok((plan, inner)) => {
                    ToolExecution::Mutation(Box::new(PreparedMutation { plan, inner }))
                }
                Err(error) => ToolExecution::Complete(ToolResult::error(error)),
            },
            ToolInput::CreateFile { path, content } => match platform::prepare_create_file(
                &self.root,
                &path,
                &content,
                operation_id,
                &stopped,
            ) {
                Ok((plan, inner)) => {
                    ToolExecution::Mutation(Box::new(PreparedMutation { plan, inner }))
                }
                Err(error) => ToolExecution::Complete(ToolResult::error(error)),
            },
            ToolInput::CreateDirectory { path } => {
                match platform::prepare_create_directory(&self.root, &path, operation_id, &stopped)
                {
                    Ok((plan, inner)) => {
                        ToolExecution::Mutation(Box::new(PreparedMutation { plan, inner }))
                    }
                    Err(error) => ToolExecution::Complete(ToolResult::error(error)),
                }
            }
            observation => ToolExecution::Observation(observation),
        }
    }

    pub(crate) fn execute_observation<F>(&self, input: &ToolInput, cancelled: &F) -> ToolResult
    where
        F: Fn() -> bool,
    {
        let deadline = Instant::now() + TOOL_OPERATION_TIMEOUT;
        let stopped = || cancelled() || Instant::now() >= deadline;
        let result = match input {
            ToolInput::ListDirectory { path, after } => {
                platform::list_directory(&self.root, path, after.as_deref(), &stopped)
            }
            ToolInput::ReadFile {
                path,
                start_line,
                line_count,
            } => platform::read_file(&self.root, path, *start_line, *line_count, &stopped),
            ToolInput::SearchText { path, query } => {
                platform::search_text(&self.root, path, query, &stopped)
            }
            ToolInput::EditFile { .. }
            | ToolInput::CreateFile { .. }
            | ToolInput::CreateDirectory { .. } => Err(ToolErrorKind::Filesystem),
        };
        result
            .map(|output| ToolResult::Ok { output })
            .unwrap_or_else(ToolResult::error)
    }

    pub(crate) fn publish_mutation(
        &self,
        prepared: Box<PreparedMutation>,
    ) -> Result<ToolResult, ToolErrorKind> {
        let PreparedMutation { plan, inner } = *prepared;
        platform::publish(&self.root, inner, &plan).map(|output| ToolResult::Ok { output })
    }

    pub(crate) fn recover_mutation(&self, encoded_plan: &[u8]) -> ToolResult {
        let plan = match decode_plan(encoded_plan) {
            Ok(plan) => plan,
            Err(_) => return ToolResult::error(ToolErrorKind::Uncertain),
        };
        match platform::recover(&self.root, &plan) {
            Ok(ToolRecoveryOutcome::Completed) => {
                let output = match plan.kind() {
                    MutationKind::EditFile => ToolOutput::FileEdited {
                        path: plan.path().clone(),
                        sha256: plan.after_sha256().unwrap_or_default().to_owned(),
                        bytes: plan.after_bytes(),
                    },
                    MutationKind::CreateFile => ToolOutput::FileCreated {
                        path: plan.path().clone(),
                        sha256: plan.after_sha256().unwrap_or_default().to_owned(),
                        bytes: plan.after_bytes(),
                    },
                    MutationKind::CreateDirectory => ToolOutput::DirectoryCreated {
                        path: plan.path().clone(),
                    },
                };
                ToolResult::Ok { output }
            }
            Ok(ToolRecoveryOutcome::NotApplied) => ToolResult::error(ToolErrorKind::Interrupted),
            Ok(ToolRecoveryOutcome::Uncertain) | Err(_) => {
                ToolResult::error(ToolErrorKind::Uncertain)
            }
        }
    }
}

impl PreparedMutation {
    pub(crate) fn encoded_plan(&self) -> Result<Vec<u8>, ToolErrorKind> {
        serde_json::to_vec(&self.plan).map_err(|_| ToolErrorKind::Filesystem)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RecoveryPlan {
    version: u16,
    kind: MutationKind,
    path: WorktreePath,
    temporary_name: String,
    parent: NodeSnapshot,
    before: Option<NodeSnapshot>,
    staged: NodeSnapshot,
    before_sha256: Option<String>,
    after_sha256: Option<String>,
    after_bytes: u64,
}

pub(super) struct RecoveryPlanParts {
    pub kind: MutationKind,
    pub path: WorktreePath,
    pub temporary_name: String,
    pub parent: NodeSnapshot,
    pub before: Option<NodeSnapshot>,
    pub staged: NodeSnapshot,
    pub before_sha256: Option<String>,
    pub after_sha256: Option<String>,
    pub after_bytes: u64,
}

impl RecoveryPlan {
    pub(super) fn from_parts(parts: RecoveryPlanParts) -> Self {
        Self {
            version: RECOVERY_PLAN_VERSION,
            kind: parts.kind,
            path: parts.path,
            temporary_name: parts.temporary_name,
            parent: parts.parent,
            before: parts.before,
            staged: parts.staged,
            before_sha256: parts.before_sha256,
            after_sha256: parts.after_sha256,
            after_bytes: parts.after_bytes,
        }
    }

    pub(super) const fn kind(&self) -> MutationKind {
        self.kind
    }

    pub(super) const fn path(&self) -> &WorktreePath {
        &self.path
    }

    pub(super) fn temporary_name(&self) -> &str {
        &self.temporary_name
    }

    pub(super) const fn parent(&self) -> &NodeSnapshot {
        &self.parent
    }

    pub(super) const fn before(&self) -> Option<&NodeSnapshot> {
        self.before.as_ref()
    }

    pub(super) const fn staged(&self) -> &NodeSnapshot {
        &self.staged
    }

    pub(super) fn before_sha256(&self) -> Option<&str> {
        self.before_sha256.as_deref()
    }

    pub(super) fn after_sha256(&self) -> Option<&str> {
        self.after_sha256.as_deref()
    }

    pub(super) const fn after_bytes(&self) -> u64 {
        self.after_bytes
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum MutationKind {
    EditFile,
    CreateFile,
    CreateDirectory,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum NodeSnapshot {
    Unix {
        device: u64,
        inode: u64,
        mode: u32,
        links: u64,
        size: u64,
        modified_seconds: i64,
        modified_nanoseconds: i64,
        changed_seconds: i64,
        changed_nanoseconds: i64,
    },
    Windows {
        volume_serial: u64,
        file_id: String,
        kind: SnapshotNodeKind,
        size: u64,
        allocation_size: u64,
        link_count: u32,
        attributes: u32,
        reparse_tag: Option<u32>,
        creation_time: i64,
        last_write_time: i64,
        change_time: i64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SnapshotNodeKind {
    File,
    Directory,
    ReparsePoint,
}

pub(crate) fn recovery_plan_is_valid(bytes: &[u8]) -> bool {
    decode_plan(bytes).is_ok()
}

fn decode_plan(bytes: &[u8]) -> Result<RecoveryPlan, ToolErrorKind> {
    if bytes.len() > super::MAX_TOOL_PAYLOAD_BYTES {
        return Err(ToolErrorKind::ResourceLimit);
    }
    let plan: RecoveryPlan =
        serde_json::from_slice(bytes).map_err(|_| ToolErrorKind::Filesystem)?;
    if plan.version != RECOVERY_PLAN_VERSION
        || plan.temporary_name.len() != ".morons-tool-".len() + 32
        || !plan.temporary_name.starts_with(".morons-tool-")
        || !plan.temporary_name[".morons-tool-".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || plan.after_bytes > MAX_FILE_BYTES
        || plan
            .before_sha256
            .as_deref()
            .is_some_and(|value| !valid_sha256(value))
        || plan
            .after_sha256
            .as_deref()
            .is_some_and(|value| !valid_sha256(value))
        || !valid_recovery_plan(&plan)
    {
        return Err(ToolErrorKind::Filesystem);
    }
    Ok(plan)
}

fn valid_recovery_plan(plan: &RecoveryPlan) -> bool {
    if plan.path.parent_and_name().is_err()
        || !valid_node_snapshot(plan.parent(), SnapshotNodeKind::Directory)
    {
        return false;
    }
    match plan.kind() {
        MutationKind::EditFile => {
            plan.before()
                .is_some_and(|before| valid_node_snapshot(before, SnapshotNodeKind::File))
                && valid_node_snapshot(plan.staged(), SnapshotNodeKind::File)
                && plan.before_sha256().is_some()
                && plan.after_sha256().is_some()
        }
        MutationKind::CreateFile => {
            plan.before().is_none()
                && valid_node_snapshot(plan.staged(), SnapshotNodeKind::File)
                && plan.before_sha256().is_none()
                && plan.after_sha256().is_some()
        }
        MutationKind::CreateDirectory => {
            plan.before().is_none()
                && valid_node_snapshot(plan.staged(), SnapshotNodeKind::Directory)
                && plan.before_sha256().is_none()
                && plan.after_sha256().is_none()
                && plan.after_bytes() == 0
        }
    }
}

fn valid_node_snapshot(snapshot: &NodeSnapshot, expected: SnapshotNodeKind) -> bool {
    match snapshot {
        NodeSnapshot::Unix {
            mode,
            links,
            size,
            modified_nanoseconds,
            changed_nanoseconds,
            ..
        } => {
            let kind = match mode & 0o170_000 {
                0o100_000 => SnapshotNodeKind::File,
                0o040_000 => SnapshotNodeKind::Directory,
                _ => return false,
            };
            kind == expected
                && *links > 0
                && (expected == SnapshotNodeKind::Directory || *size <= MAX_FILE_BYTES)
                && (0..1_000_000_000).contains(modified_nanoseconds)
                && (0..1_000_000_000).contains(changed_nanoseconds)
        }
        NodeSnapshot::Windows {
            file_id,
            kind,
            size,
            reparse_tag,
            ..
        } => {
            *kind == expected
                && *kind != SnapshotNodeKind::ReparsePoint
                && reparse_tag.is_none()
                && (expected == SnapshotNodeKind::Directory || *size <= MAX_FILE_BYTES)
                && file_id.len() == 32
                && file_id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }
    }
}

pub(super) fn temporary_name(operation_id: [u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut name = String::with_capacity(".morons-tool-".len() + 32);
    name.push_str(".morons-tool-");
    for byte in operation_id {
        name.push(char::from(HEX[usize::from(byte >> 4)]));
        name.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    name
}

pub(super) fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    hex(&digest)
}

pub(super) fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

pub(super) fn build_read_output(
    path: WorktreePath,
    bytes: Vec<u8>,
    start_line: u32,
    line_count: u16,
) -> Result<ToolOutput, ToolErrorKind> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(ToolErrorKind::ResourceLimit);
    }
    if bytes.contains(&0) {
        return Err(ToolErrorKind::BinaryFile);
    }
    let text = String::from_utf8(bytes).map_err(|_| ToolErrorKind::InvalidUtf8)?;
    let digest = sha256_hex(text.as_bytes());
    let lines = text.split_inclusive('\n').collect::<Vec<_>>();
    let start = usize::try_from(start_line - 1).map_err(|_| ToolErrorKind::ResourceLimit)?;
    let end = start
        .checked_add(usize::from(line_count))
        .map(|value| value.min(lines.len()))
        .ok_or(ToolErrorKind::ResourceLimit)?;
    let selected = if start < lines.len() {
        lines[start..end].concat()
    } else {
        String::new()
    };
    if selected.len() > MAX_READ_OUTPUT_BYTES {
        return Err(ToolErrorKind::ResourceLimit);
    }
    let returned_lines = if start < lines.len() { end - start } else { 0 };
    let end_line = start_line
        .checked_add(u32::try_from(returned_lines).map_err(|_| ToolErrorKind::ResourceLimit)?)
        .and_then(|value| value.checked_sub(1))
        .unwrap_or(start_line.saturating_sub(1));
    Ok(ToolOutput::FileRead {
        path,
        start_line,
        end_line,
        end_of_file: end >= lines.len(),
        sha256: digest,
        text: selected,
    })
}

pub(super) fn apply_replacements(
    source: &str,
    replacements: &[TextReplacement],
) -> Result<String, ToolErrorKind> {
    if replacements.is_empty() || replacements.len() > MAX_REPLACEMENTS {
        return Err(ToolErrorKind::ResourceLimit);
    }
    let replacement_bytes = replacements.iter().try_fold(0_usize, |total, replacement| {
        total
            .checked_add(replacement.old_text.len())?
            .checked_add(replacement.new_text.len())
    });
    if replacement_bytes.is_none_or(|bytes| bytes > MAX_REPLACEMENT_BYTES) {
        return Err(ToolErrorKind::ResourceLimit);
    }
    if replacements
        .iter()
        .any(|replacement| replacement.old_text.is_empty())
    {
        if source.is_empty()
            && replacements.len() == 1
            && replacements[0].old_text.is_empty()
            && replacements[0].new_text.len() as u64 <= MAX_FILE_BYTES
        {
            return Ok(replacements[0].new_text.clone());
        }
        return Err(ToolErrorKind::ReplacementAmbiguous);
    }

    let mut ranges = Vec::with_capacity(replacements.len());
    for replacement in replacements {
        let mut matches = source.match_indices(&replacement.old_text);
        let Some((start, _)) = matches.next() else {
            return Err(ToolErrorKind::ReplacementNotFound);
        };
        if matches.next().is_some() {
            return Err(ToolErrorKind::ReplacementAmbiguous);
        }
        let end = start
            .checked_add(replacement.old_text.len())
            .ok_or(ToolErrorKind::ResourceLimit)?;
        ranges.push((start, end, replacement.new_text.as_str()));
    }
    ranges.sort_by_key(|(start, _, _)| *start);
    if ranges.windows(2).any(|ranges| ranges[0].1 > ranges[1].0) {
        return Err(ToolErrorKind::ReplacementOverlap);
    }
    let resulting_bytes = source
        .len()
        .checked_sub(ranges.iter().map(|(start, end, _)| end - start).sum())
        .and_then(|bytes| bytes.checked_add(ranges.iter().map(|(_, _, text)| text.len()).sum()))
        .ok_or(ToolErrorKind::ResourceLimit)?;
    if resulting_bytes as u64 > MAX_FILE_BYTES {
        return Err(ToolErrorKind::ResourceLimit);
    }
    let mut result = String::with_capacity(resulting_bytes);
    let mut previous = 0;
    for (start, end, replacement) in ranges {
        result.push_str(&source[previous..start]);
        result.push_str(replacement);
        previous = end;
    }
    result.push_str(&source[previous..]);
    Ok(result)
}

pub(super) fn directory_output(
    path: WorktreePath,
    mut entries: Vec<DirectoryListEntry>,
    after: Option<&str>,
) -> ToolOutput {
    entries.sort_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()));
    let start = after.map_or(0, |after| {
        entries.partition_point(|entry| entry.name.as_bytes() <= after.as_bytes())
    });
    let end = start
        .saturating_add(MAX_DIRECTORY_ENTRIES)
        .min(entries.len());
    ToolOutput::DirectoryPage {
        path,
        entries: entries[start..end].to_vec(),
        has_more: end < entries.len(),
    }
}

pub(super) fn classify_kind(
    file: bool,
    directory: bool,
) -> Result<DirectoryEntryKind, ToolErrorKind> {
    match (file, directory) {
        (true, false) => Ok(DirectoryEntryKind::File),
        (false, true) => Ok(DirectoryEntryKind::Directory),
        _ => Err(ToolErrorKind::WrongNodeKind),
    }
}

pub(super) fn same_node_identity(left: &NodeSnapshot, right: &NodeSnapshot) -> bool {
    match (left, right) {
        (
            NodeSnapshot::Unix {
                device: left_device,
                inode: left_inode,
                mode: left_mode,
                ..
            },
            NodeSnapshot::Unix {
                device: right_device,
                inode: right_inode,
                mode: right_mode,
                ..
            },
        ) => left_device == right_device && left_inode == right_inode && left_mode == right_mode,
        (
            NodeSnapshot::Windows {
                volume_serial: left_volume,
                file_id: left_id,
                kind: left_kind,
                ..
            },
            NodeSnapshot::Windows {
                volume_serial: right_volume,
                file_id: right_id,
                kind: right_kind,
                ..
            },
        ) => left_volume == right_volume && left_id == right_id && left_kind == right_kind,
        _ => false,
    }
}

pub(super) fn same_published_node(left: &NodeSnapshot, staged: &NodeSnapshot) -> bool {
    match (left, staged) {
        (
            NodeSnapshot::Unix {
                device: left_device,
                inode: left_inode,
                mode: left_mode,
                size: left_size,
                ..
            },
            NodeSnapshot::Unix {
                device: right_device,
                inode: right_inode,
                mode: right_mode,
                size: right_size,
                ..
            },
        ) => {
            left_device == right_device
                && left_inode == right_inode
                && left_mode == right_mode
                && left_size == right_size
        }
        (
            NodeSnapshot::Windows {
                volume_serial: left_volume,
                file_id: left_id,
                kind: left_kind,
                size: left_size,
                reparse_tag: left_reparse,
                ..
            },
            NodeSnapshot::Windows {
                volume_serial: right_volume,
                file_id: right_id,
                kind: right_kind,
                size: right_size,
                reparse_tag: right_reparse,
                ..
            },
        ) => {
            left_volume == right_volume
                && left_id == right_id
                && left_kind == right_kind
                && left_size == right_size
                && left_reparse == right_reparse
        }
        _ => false,
    }
}

pub(super) fn bounded_match_text(line: &str) -> String {
    const MAX_FRAGMENT_BYTES: usize = 1_024;
    if line.len() <= MAX_FRAGMENT_BYTES {
        return line.to_owned();
    }
    let mut end = MAX_FRAGMENT_BYTES;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].to_owned()
}

pub(super) struct SearchState {
    pub(super) matches: Vec<SearchMatch>,
    pub(super) skipped_binary_files: u32,
    pub(super) files_scanned: u32,
    pub(super) bytes_scanned: u64,
    pub(super) output_bytes: usize,
    pub(super) truncated: bool,
}

impl SearchState {
    pub(super) const fn new() -> Self {
        Self {
            matches: Vec::new(),
            skipped_binary_files: 0,
            files_scanned: 0,
            bytes_scanned: 0,
            output_bytes: 0,
            truncated: false,
        }
    }
}

pub(super) fn root_is_directory(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.file_type().is_dir() && !metadata.file_type().is_symlink())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, process};

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::tools::{TextReplacement, ToolInput};

    #[test]
    fn structured_tools_read_search_create_and_edit_one_worktree() {
        let root = TestWorktree::new("complete-loop");
        root.write("src/lib.rs", "alpha\nbeta\nalpha tail\n");
        let executor = WorktreeToolExecutor::new(root.path.clone());

        let listing = executor.execute_observation(
            &ToolInput::ListDirectory {
                path: WorktreePath::parse(".", true).expect("root path should parse"),
                after: None,
            },
            &|| false,
        );
        assert!(
            matches!(listing, ToolResult::Ok { output: ToolOutput::DirectoryPage { ref entries, has_more: false, .. } } if entries.len() == 1)
        );

        let read = executor.execute_observation(
            &ToolInput::ReadFile {
                path: WorktreePath::parse("src/lib.rs", false).expect("file path should parse"),
                start_line: 2,
                line_count: 2,
            },
            &|| false,
        );
        let ToolResult::Ok {
            output: ToolOutput::FileRead { sha256, text, .. },
        } = read
        else {
            panic!("file should be read");
        };
        assert_eq!(text, "beta\nalpha tail\n");

        let search = executor.execute_observation(
            &ToolInput::SearchText {
                path: WorktreePath::parse(".", true).expect("root path should parse"),
                query: "alpha".to_owned(),
            },
            &|| false,
        );
        assert!(
            matches!(search, ToolResult::Ok { output: ToolOutput::SearchMatches { ref matches, truncated: false, .. } } if matches.len() == 2)
        );

        let edit = ToolInput::EditFile {
            path: WorktreePath::parse("src/lib.rs", false).expect("file path should parse"),
            expected_sha256: sha256,
            replacements: vec![TextReplacement {
                old_text: "beta".to_owned(),
                new_text: "gamma".to_owned(),
            }],
        };
        let ToolExecution::Mutation(prepared) = executor.prepare(edit, [0x11; 16], &|| false)
        else {
            panic!("edit should prepare");
        };
        let plan = prepared.encoded_plan().expect("plan should encode");
        let published = executor.publish_mutation(prepared);
        assert!(
            matches!(
                published,
                Ok(ToolResult::Ok {
                    output: ToolOutput::FileEdited { .. }
                })
            ),
            "unexpected edit publication: {published:?}"
        );
        assert_eq!(
            fs::read_to_string(root.path.join("src/lib.rs")).expect("edited file"),
            "alpha\ngamma\nalpha tail\n"
        );
        assert!(matches!(
            executor.recover_mutation(&plan),
            ToolResult::Ok {
                output: ToolOutput::FileEdited { .. }
            }
        ));

        let create = ToolInput::CreateFile {
            path: WorktreePath::parse("src/new.rs", false).expect("new path should parse"),
            content: "new file\n".to_owned(),
        };
        let ToolExecution::Mutation(prepared) = executor.prepare(create, [0x22; 16], &|| false)
        else {
            panic!("create should prepare");
        };
        executor
            .publish_mutation(prepared)
            .expect("file should publish");
        assert_eq!(
            fs::read_to_string(root.path.join("src/new.rs")).expect("new file"),
            "new file\n"
        );

        let directory = ToolInput::CreateDirectory {
            path: WorktreePath::parse("tests", false).expect("directory path should parse"),
        };
        let ToolExecution::Mutation(prepared) = executor.prepare(directory, [0x33; 16], &|| false)
        else {
            panic!("directory should prepare");
        };
        executor
            .publish_mutation(prepared)
            .expect("directory should publish");
        assert!(root.path.join("tests").is_dir());
    }

    #[test]
    fn stale_ambiguous_and_cancelled_tools_fail_without_writing() {
        let root = TestWorktree::new("fail-closed");
        root.write("file.txt", "same same\n");
        let executor = WorktreeToolExecutor::new(root.path.clone());
        let digest = sha256_hex(b"same same\n");
        let ambiguous = ToolInput::EditFile {
            path: WorktreePath::parse("file.txt", false).expect("path should parse"),
            expected_sha256: digest,
            replacements: vec![TextReplacement {
                old_text: "same".to_owned(),
                new_text: "other".to_owned(),
            }],
        };
        assert!(matches!(
            executor.prepare(ambiguous, [0x44; 16], &|| false),
            ToolExecution::Complete(ToolResult::Error {
                error: ToolErrorKind::ReplacementAmbiguous
            })
        ));
        assert!(matches!(
            executor.execute_observation(
                &ToolInput::ReadFile {
                    path: WorktreePath::parse("file.txt", false).expect("path should parse"),
                    start_line: 1,
                    line_count: 1,
                },
                &|| true,
            ),
            ToolResult::Error {
                error: ToolErrorKind::Cancelled
            }
        ));
        assert_eq!(
            fs::read_to_string(root.path.join("file.txt")).expect("original file"),
            "same same\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn links_are_not_in_the_tool_capability_graph() {
        let root = TestWorktree::new("link-rejection");
        root.write("outside.txt", "outside\n");
        std::os::unix::fs::symlink(root.path.join("outside.txt"), root.path.join("link.txt"))
            .expect("test link should be created");
        let executor = WorktreeToolExecutor::new(root.path.clone());
        let result = executor.execute_observation(
            &ToolInput::ReadFile {
                path: WorktreePath::parse("link.txt", false).expect("path should parse"),
                start_line: 1,
                line_count: 1,
            },
            &|| false,
        );
        assert!(matches!(result, ToolResult::Error { .. }));
    }

    struct TestWorktree {
        path: PathBuf,
    }

    impl TestWorktree {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "morons-tools-{label}-{}-{:?}",
                process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&path);
            create_directory(&path);
            Self { path }
        }

        fn write(&self, relative: &str, content: &str) {
            let path = self.path.join(relative);
            if let Some(parent) = path.parent() {
                create_directory(parent);
            }
            fs::write(&path, content).expect("test file should be written");
            #[cfg(unix)]
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                .expect("test file should be private");
        }
    }

    impl Drop for TestWorktree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn create_directory(path: &Path) {
        fs::create_dir_all(path).expect("test directory should be created");
        #[cfg(unix)]
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .expect("test directory should be private");
    }
}
