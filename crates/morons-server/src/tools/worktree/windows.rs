use std::{
    ffi::OsStr,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

use fence_windows::{
    DirectoryHandle, MutationRoot, NodeHandle, NodeKind, NodeMetadata, RootHandle, WindowsError,
};

use super::{
    MutationKind, NodeSnapshot, RecoveryPlan, RecoveryPlanParts, SearchState, SnapshotNodeKind,
    ToolRecoveryOutcome, apply_replacements, bounded_match_text, build_read_output, classify_kind,
    directory_output, hex, root_is_directory, same_node_identity, same_published_node, sha256_hex,
    temporary_name,
};
use crate::tools::{
    DirectoryListEntry, MAX_FILE_BYTES, MAX_SEARCH_BYTES, MAX_SEARCH_FILES, MAX_SEARCH_MATCHES,
    MAX_SEARCH_OUTPUT_BYTES, SearchMatch, TextReplacement, ToolErrorKind, ToolOutput, WorktreePath,
};

pub(super) struct PreparedMutation {
    parent: DirectoryHandle,
    target: Option<NodeHandle>,
    published: bool,
    temporary_name: String,
}

impl Drop for PreparedMutation {
    fn drop(&mut self) {
        if !self.published {
            let _ = self.parent.remove_child(OsStr::new(&self.temporary_name));
        }
    }
}

pub(super) fn list_directory<F>(
    root: &Path,
    path: &WorktreePath,
    after: Option<&str>,
    cancelled: &F,
) -> Result<ToolOutput, ToolErrorKind>
where
    F: Fn() -> bool,
{
    let directory = open_directory(root, path)?;
    let before = snapshot_metadata(directory.metadata());
    let entries_before = directory.entries().map_err(map_windows)?;
    let mut entries = Vec::new();
    for entry in entries_before.clone() {
        if cancelled() {
            return Err(ToolErrorKind::Cancelled);
        }
        let name = entry
            .name
            .to_str()
            .ok_or(ToolErrorKind::InvalidPath)?
            .to_owned();
        WorktreePath::parse(&name, false)?;
        let child = directory.open_child(&entry).map_err(map_windows)?;
        reject_unsupported_node(&child)?;
        let metadata = child.metadata();
        let kind = classify_kind(
            metadata.kind == NodeKind::RegularFile,
            metadata.kind == NodeKind::Directory,
        )?;
        entries.push(DirectoryListEntry { name, kind });
    }
    if snapshot_metadata(directory.metadata()) != before
        || !same_raw_entries(&entries_before, &directory.entries().map_err(map_windows)?)
    {
        return Err(ToolErrorKind::ChangedDuringOperation);
    }
    Ok(directory_output(path.clone(), entries, after))
}

pub(super) fn read_file<F>(
    root: &Path,
    path: &WorktreePath,
    start_line: u32,
    line_count: u16,
    cancelled: &F,
) -> Result<ToolOutput, ToolErrorKind>
where
    F: Fn() -> bool,
{
    let node = open_file(root, path)?;
    let before = node.metadata();
    reject_unsupported_node(&node)?;
    let mut file = node.try_clone_file().map_err(map_windows)?;
    let bytes = read_bounded(&mut file, before.size, cancelled)?;
    if node.refresh_metadata().map_err(map_windows)? != before
        || node.verify_path_identity().is_err()
    {
        return Err(ToolErrorKind::ChangedDuringOperation);
    }
    build_read_output(path.clone(), bytes, start_line, line_count)
}

pub(super) fn search_text<F>(
    root: &Path,
    path: &WorktreePath,
    query: &str,
    cancelled: &F,
) -> Result<ToolOutput, ToolErrorKind>
where
    F: Fn() -> bool,
{
    let directory = open_directory(root, path)?;
    let mut state = SearchState::new();
    search_directory(&directory, path, query, &mut state, cancelled)?;
    Ok(ToolOutput::SearchMatches {
        path: path.clone(),
        matches: state.matches,
        skipped_binary_files: state.skipped_binary_files,
        truncated: state.truncated,
    })
}

pub(super) fn prepare_edit<F>(
    root: &Path,
    path: &WorktreePath,
    expected_sha256: &str,
    replacements: &[TextReplacement],
    operation_id: [u8; 16],
    cancelled: &F,
) -> Result<(RecoveryPlan, PreparedMutation), ToolErrorKind>
where
    F: Fn() -> bool,
{
    let (parent_path, name) = path.parent_and_name()?;
    let parent = open_mutation_directory(root, &parent_path)?;
    let target = open_exact_child(&parent, name, Some(ExpectedKind::File))?;
    reject_unsupported_node(&target)?;
    let before = target.metadata();
    let mut target_file = target.try_clone_file().map_err(map_windows)?;
    let bytes = read_bounded(&mut target_file, before.size, cancelled)?;
    if target.refresh_metadata().map_err(map_windows)? != before
        || target.verify_path_identity().is_err()
    {
        return Err(ToolErrorKind::ChangedDuringOperation);
    }
    if bytes.contains(&0) {
        return Err(ToolErrorKind::BinaryFile);
    }
    let source = String::from_utf8(bytes).map_err(|_| ToolErrorKind::InvalidUtf8)?;
    let before_digest = sha256_hex(source.as_bytes());
    if before_digest != expected_sha256 {
        return Err(ToolErrorKind::DigestMismatch);
    }
    let result = apply_replacements(&source, replacements)?;
    if cancelled() {
        return Err(ToolErrorKind::Cancelled);
    }
    let after_digest = sha256_hex(result.as_bytes());
    let temporary_name = temporary_name(operation_id);
    let mut staged_file = parent
        .create_new_file(OsStr::new(&temporary_name))
        .map_err(map_create_windows)?;
    staged_file.write_all(result.as_bytes()).map_err(map_io)?;
    staged_file.sync_all().map_err(map_io)?;
    let staged = open_exact_child(&parent, &temporary_name, Some(ExpectedKind::File))?;
    reject_unsupported_node(&staged)?;
    verify_file(&staged, &after_digest)?;
    let plan = RecoveryPlan::from_parts(RecoveryPlanParts {
        kind: MutationKind::EditFile,
        path: path.clone(),
        temporary_name: temporary_name.clone(),
        parent: snapshot_metadata(parent.metadata()),
        before: Some(snapshot_metadata(before)),
        staged: snapshot_metadata(staged.metadata()),
        before_sha256: Some(before_digest),
        after_sha256: Some(after_digest),
        after_bytes: result.len() as u64,
    });
    Ok((
        plan,
        PreparedMutation {
            parent,
            target: Some(target),
            published: false,
            temporary_name,
        },
    ))
}

pub(super) fn prepare_create_file<F>(
    root: &Path,
    path: &WorktreePath,
    content: &str,
    operation_id: [u8; 16],
    cancelled: &F,
) -> Result<(RecoveryPlan, PreparedMutation), ToolErrorKind>
where
    F: Fn() -> bool,
{
    if content.len() as u64 > MAX_FILE_BYTES {
        return Err(ToolErrorKind::ResourceLimit);
    }
    let (parent_path, name) = path.parent_and_name()?;
    let parent = open_mutation_directory(root, &parent_path)?;
    require_name_absent(&parent, name)?;
    if cancelled() {
        return Err(ToolErrorKind::Cancelled);
    }
    let temporary_name = temporary_name(operation_id);
    let mut staged_file = parent
        .create_new_file(OsStr::new(&temporary_name))
        .map_err(map_create_windows)?;
    staged_file.write_all(content.as_bytes()).map_err(map_io)?;
    staged_file.sync_all().map_err(map_io)?;
    let staged = open_exact_child(&parent, &temporary_name, Some(ExpectedKind::File))?;
    reject_unsupported_node(&staged)?;
    let digest = sha256_hex(content.as_bytes());
    verify_file(&staged, &digest)?;
    let plan = RecoveryPlan::from_parts(RecoveryPlanParts {
        kind: MutationKind::CreateFile,
        path: path.clone(),
        temporary_name: temporary_name.clone(),
        parent: snapshot_metadata(parent.metadata()),
        before: None,
        staged: snapshot_metadata(staged.metadata()),
        before_sha256: None,
        after_sha256: Some(digest),
        after_bytes: content.len() as u64,
    });
    Ok((
        plan,
        PreparedMutation {
            parent,
            target: None,
            published: false,
            temporary_name,
        },
    ))
}

pub(super) fn prepare_create_directory<F>(
    root: &Path,
    path: &WorktreePath,
    operation_id: [u8; 16],
    cancelled: &F,
) -> Result<(RecoveryPlan, PreparedMutation), ToolErrorKind>
where
    F: Fn() -> bool,
{
    let (parent_path, name) = path.parent_and_name()?;
    let parent = open_mutation_directory(root, &parent_path)?;
    require_name_absent(&parent, name)?;
    if cancelled() {
        return Err(ToolErrorKind::Cancelled);
    }
    let temporary_name = temporary_name(operation_id);
    parent
        .create_new_directory(OsStr::new(&temporary_name))
        .map_err(map_create_windows)?;
    let staged = open_exact_child(&parent, &temporary_name, Some(ExpectedKind::Directory))?;
    let plan = RecoveryPlan::from_parts(RecoveryPlanParts {
        kind: MutationKind::CreateDirectory,
        path: path.clone(),
        temporary_name: temporary_name.clone(),
        parent: snapshot_metadata(parent.metadata()),
        before: None,
        staged: snapshot_metadata(staged.metadata()),
        before_sha256: None,
        after_sha256: None,
        after_bytes: 0,
    });
    Ok((
        plan,
        PreparedMutation {
            parent,
            target: None,
            published: false,
            temporary_name,
        },
    ))
}

pub(super) fn publish(
    _root: &Path,
    mut prepared: PreparedMutation,
    plan: &RecoveryPlan,
) -> Result<ToolOutput, ToolErrorKind> {
    if !same_node_identity(
        &snapshot_metadata(prepared.parent.metadata()),
        plan.parent(),
    ) {
        return Err(ToolErrorKind::ChangedDuringOperation);
    }
    let (_, name) = plan.path().parent_and_name()?;
    match plan.kind() {
        MutationKind::EditFile => {
            let retained = prepared.target.take().ok_or(ToolErrorKind::Filesystem)?;
            if Some(&snapshot_metadata(retained.metadata())) != plan.before()
                || retained.refresh_metadata().map_err(map_windows)? != retained.metadata()
                || retained.verify_path_identity().is_err()
            {
                return Err(ToolErrorKind::ChangedDuringOperation);
            }
            let current = open_exact_child(&prepared.parent, name, Some(ExpectedKind::File))?;
            if Some(&snapshot_metadata(current.metadata())) != plan.before() {
                return Err(ToolErrorKind::ChangedDuringOperation);
            }
            verify_file(
                &current,
                plan.before_sha256().ok_or(ToolErrorKind::Filesystem)?,
            )?;
            let staged = open_exact_child(
                &prepared.parent,
                plan.temporary_name(),
                Some(ExpectedKind::File),
            )?;
            if snapshot_metadata(staged.metadata()) != *plan.staged() {
                return Err(ToolErrorKind::ChangedDuringOperation);
            }
            drop(staged);
            drop(current);
            drop(retained);
            prepared
                .parent
                .replace_child(OsStr::new(plan.temporary_name()), OsStr::new(name))
                .map_err(map_windows)?;
        }
        MutationKind::CreateFile | MutationKind::CreateDirectory => {
            require_name_absent(&prepared.parent, name)?;
            let expected = if plan.kind() == MutationKind::CreateFile {
                ExpectedKind::File
            } else {
                ExpectedKind::Directory
            };
            let staged = open_exact_child(&prepared.parent, plan.temporary_name(), Some(expected))?;
            if snapshot_metadata(staged.metadata()) != *plan.staged() {
                return Err(ToolErrorKind::ChangedDuringOperation);
            }
            drop(staged);
            prepared
                .parent
                .rename_child_noreplace(OsStr::new(plan.temporary_name()), OsStr::new(name))
                .map_err(map_create_windows)?;
        }
    }
    prepared.published = true;
    let published = open_exact_child(
        &prepared.parent,
        name,
        Some(match plan.kind() {
            MutationKind::EditFile | MutationKind::CreateFile => ExpectedKind::File,
            MutationKind::CreateDirectory => ExpectedKind::Directory,
        }),
    )?;
    if !same_published_node(&snapshot_metadata(published.metadata()), plan.staged()) {
        return Err(ToolErrorKind::ChangedDuringOperation);
    }
    match plan.kind() {
        MutationKind::EditFile => {
            let digest = plan.after_sha256().ok_or(ToolErrorKind::Filesystem)?;
            verify_file(&published, digest)?;
            Ok(ToolOutput::FileEdited {
                path: plan.path().clone(),
                sha256: digest.to_owned(),
                bytes: plan.after_bytes(),
            })
        }
        MutationKind::CreateFile => {
            let digest = plan.after_sha256().ok_or(ToolErrorKind::Filesystem)?;
            verify_file(&published, digest)?;
            Ok(ToolOutput::FileCreated {
                path: plan.path().clone(),
                sha256: digest.to_owned(),
                bytes: plan.after_bytes(),
            })
        }
        MutationKind::CreateDirectory => Ok(ToolOutput::DirectoryCreated {
            path: plan.path().clone(),
        }),
    }
}

pub(super) fn recover(
    root: &Path,
    plan: &RecoveryPlan,
) -> Result<ToolRecoveryOutcome, ToolErrorKind> {
    let (parent_path, name) = plan.path().parent_and_name()?;
    let parent = open_mutation_directory(root, &parent_path)?;
    if !same_node_identity(&snapshot_metadata(parent.metadata()), plan.parent()) {
        return Ok(ToolRecoveryOutcome::Uncertain);
    }
    let target = open_optional_child(&parent, name)?;
    let temporary = open_optional_child(&parent, plan.temporary_name())?;
    let target_is_after = target.as_ref().is_some_and(|node| {
        same_published_node(&snapshot_metadata(node.metadata()), plan.staged())
    });
    let target_is_before = match (target.as_ref(), plan.before()) {
        (Some(node), Some(before)) => snapshot_metadata(node.metadata()) == *before,
        (None, None) => true,
        _ => false,
    };
    let temporary_is_staged = temporary
        .as_ref()
        .is_some_and(|node| snapshot_metadata(node.metadata()) == *plan.staged());
    if target_is_after && temporary.is_none() {
        if matches!(
            plan.kind(),
            MutationKind::EditFile | MutationKind::CreateFile
        ) && verify_file(
            target.as_ref().ok_or(ToolErrorKind::Filesystem)?,
            plan.after_sha256().ok_or(ToolErrorKind::Filesystem)?,
        )
        .is_err()
        {
            return Ok(ToolRecoveryOutcome::Uncertain);
        }
        return Ok(ToolRecoveryOutcome::Completed);
    }
    if target_is_before && (temporary.is_none() || temporary_is_staged) {
        drop(target);
        drop(temporary);
        if temporary_is_staged {
            parent
                .remove_child(OsStr::new(plan.temporary_name()))
                .map_err(map_windows)?;
        }
        return Ok(ToolRecoveryOutcome::NotApplied);
    }
    Ok(ToolRecoveryOutcome::Uncertain)
}

fn search_directory<F>(
    directory: &DirectoryHandle,
    relative: &WorktreePath,
    query: &str,
    state: &mut SearchState,
    cancelled: &F,
) -> Result<(), ToolErrorKind>
where
    F: Fn() -> bool,
{
    let before = directory.entries().map_err(map_windows)?;
    let mut entries = before.clone();
    entries.sort_by(|left, right| {
        left.name
            .as_encoded_bytes()
            .cmp(right.name.as_encoded_bytes())
    });
    for entry in entries {
        if cancelled() {
            return Err(ToolErrorKind::Cancelled);
        }
        if state.truncated {
            break;
        }
        let name = entry
            .name
            .to_str()
            .ok_or(ToolErrorKind::InvalidPath)?
            .to_owned();
        let child_relative = relative.join_component(&name)?;
        let child = directory.open_child(&entry).map_err(map_windows)?;
        reject_unsupported_node(&child)?;
        match child.metadata().kind {
            NodeKind::Directory => search_directory(
                &child.into_directory().map_err(map_windows)?,
                &child_relative,
                query,
                state,
                cancelled,
            )?,
            NodeKind::RegularFile => search_file(&child, child_relative, query, state, cancelled)?,
            NodeKind::ReparsePoint => return Err(ToolErrorKind::LinkOrReparsePoint),
        }
    }
    if !same_raw_entries(&before, &directory.entries().map_err(map_windows)?) {
        return Err(ToolErrorKind::ChangedDuringOperation);
    }
    Ok(())
}

fn search_file<F>(
    node: &NodeHandle,
    relative: WorktreePath,
    query: &str,
    state: &mut SearchState,
    cancelled: &F,
) -> Result<(), ToolErrorKind>
where
    F: Fn() -> bool,
{
    let before = node.metadata();
    let size = before.size;
    if size > MAX_FILE_BYTES
        || state.files_scanned >= MAX_SEARCH_FILES
        || state
            .bytes_scanned
            .checked_add(size)
            .is_none_or(|total| total > MAX_SEARCH_BYTES)
    {
        state.truncated = true;
        return Ok(());
    }
    let mut file = node.try_clone_file().map_err(map_windows)?;
    let bytes = read_bounded(&mut file, size, cancelled)?;
    if node.refresh_metadata().map_err(map_windows)? != before
        || node.verify_path_identity().is_err()
    {
        return Err(ToolErrorKind::ChangedDuringOperation);
    }
    state.files_scanned += 1;
    state.bytes_scanned += size;
    if bytes.contains(&0) {
        state.skipped_binary_files = state.skipped_binary_files.saturating_add(1);
        return Ok(());
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        state.skipped_binary_files = state.skipped_binary_files.saturating_add(1);
        return Ok(());
    };
    for (index, line) in text.lines().enumerate() {
        if !line.contains(query) {
            continue;
        }
        let fragment = bounded_match_text(line);
        let added = relative
            .as_str()
            .len()
            .checked_add(fragment.len())
            .and_then(|value| value.checked_add(16))
            .ok_or(ToolErrorKind::ResourceLimit)?;
        if state.matches.len() >= MAX_SEARCH_MATCHES
            || state
                .output_bytes
                .checked_add(added)
                .is_none_or(|bytes| bytes > MAX_SEARCH_OUTPUT_BYTES)
        {
            state.truncated = true;
            break;
        }
        state.output_bytes += added;
        state.matches.push(SearchMatch {
            path: relative.clone(),
            line: u32::try_from(index + 1).map_err(|_| ToolErrorKind::ResourceLimit)?,
            text: fragment,
        });
    }
    Ok(())
}

fn open_directory(root: &Path, path: &WorktreePath) -> Result<DirectoryHandle, ToolErrorKind> {
    if !root_is_directory(root) {
        return Err(ToolErrorKind::WrongNodeKind);
    }
    let mut directory = RootHandle::open(root)
        .map_err(map_windows)?
        .into_directory();
    for component in path.components() {
        let node = open_exact_child(&directory, component, Some(ExpectedKind::Directory))?;
        directory = node.into_directory().map_err(map_windows)?;
    }
    Ok(directory)
}

fn open_mutation_directory(
    root: &Path,
    path: &WorktreePath,
) -> Result<DirectoryHandle, ToolErrorKind> {
    if !root_is_directory(root) {
        return Err(ToolErrorKind::WrongNodeKind);
    }
    let mut directory = MutationRoot::open(root)
        .map_err(map_windows)?
        .into_directory();
    for component in path.components() {
        directory = directory
            .open_mutation_directory(OsStr::new(component))
            .map_err(map_windows)?;
    }
    Ok(directory)
}

fn open_file(root: &Path, path: &WorktreePath) -> Result<NodeHandle, ToolErrorKind> {
    let (parent, name) = path.parent_and_name()?;
    let directory = open_directory(root, &parent)?;
    open_exact_child(&directory, name, Some(ExpectedKind::File))
}

fn open_exact_child(
    directory: &DirectoryHandle,
    name: &str,
    expected: Option<ExpectedKind>,
) -> Result<NodeHandle, ToolErrorKind> {
    let entry = directory
        .entries()
        .map_err(map_windows)?
        .into_iter()
        .find(|entry| entry.name == OsStr::new(name))
        .ok_or(ToolErrorKind::NotFound)?;
    let node = directory.open_child(&entry).map_err(map_windows)?;
    reject_unsupported_node(&node)?;
    match expected {
        Some(ExpectedKind::File) if node.metadata().kind != NodeKind::RegularFile => {
            Err(ToolErrorKind::WrongNodeKind)
        }
        Some(ExpectedKind::Directory) if node.metadata().kind != NodeKind::Directory => {
            Err(ToolErrorKind::WrongNodeKind)
        }
        _ => Ok(node),
    }
}

fn open_optional_child(
    directory: &DirectoryHandle,
    name: &str,
) -> Result<Option<NodeHandle>, ToolErrorKind> {
    let entry = directory
        .entries()
        .map_err(map_windows)?
        .into_iter()
        .find(|entry| entry.name == OsStr::new(name));
    entry
        .map(|entry| directory.open_child(&entry).map_err(map_windows))
        .transpose()
}

fn require_name_absent(directory: &DirectoryHandle, name: &str) -> Result<(), ToolErrorKind> {
    if directory
        .entries()
        .map_err(map_windows)?
        .iter()
        .any(|entry| entry.name == OsStr::new(name))
    {
        Err(ToolErrorKind::AlreadyExists)
    } else {
        Ok(())
    }
}

fn reject_unsupported_node(node: &NodeHandle) -> Result<(), ToolErrorKind> {
    let metadata = node.metadata();
    if metadata.kind == NodeKind::ReparsePoint || metadata.reparse_tag.is_some() {
        return Err(ToolErrorKind::LinkOrReparsePoint);
    }
    if metadata.kind == NodeKind::RegularFile {
        let streams = node.streams().map_err(map_windows)?;
        if streams.len() != 1 || !streams[0].is_default_data_stream() {
            return Err(ToolErrorKind::LinkOrReparsePoint);
        }
    }
    Ok(())
}

fn verify_file(node: &NodeHandle, expected_digest: &str) -> Result<(), ToolErrorKind> {
    reject_unsupported_node(node)?;
    let before = node.metadata();
    let mut file = node.try_clone_file().map_err(map_windows)?;
    let bytes = read_bounded(&mut file, before.size, &|| false)?;
    if node.refresh_metadata().map_err(map_windows)? != before
        || node.verify_path_identity().is_err()
        || sha256_hex(&bytes) != expected_digest
    {
        return Err(ToolErrorKind::ChangedDuringOperation);
    }
    Ok(())
}

fn read_bounded<F>(
    file: &mut File,
    expected_size: u64,
    cancelled: &F,
) -> Result<Vec<u8>, ToolErrorKind>
where
    F: Fn() -> bool,
{
    if expected_size > MAX_FILE_BYTES {
        return Err(ToolErrorKind::ResourceLimit);
    }
    file.seek(SeekFrom::Start(0)).map_err(map_io)?;
    let mut bytes = Vec::with_capacity(
        usize::try_from(expected_size).map_err(|_| ToolErrorKind::ResourceLimit)?,
    );
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        if cancelled() {
            return Err(ToolErrorKind::Cancelled);
        }
        let read = file.read(&mut buffer).map_err(map_io)?;
        if read == 0 {
            break;
        }
        if bytes
            .len()
            .checked_add(read)
            .is_none_or(|length| length as u64 > MAX_FILE_BYTES)
        {
            return Err(ToolErrorKind::ResourceLimit);
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    if bytes.len() as u64 != expected_size {
        return Err(ToolErrorKind::ChangedDuringOperation);
    }
    Ok(bytes)
}

fn snapshot_metadata(metadata: NodeMetadata) -> NodeSnapshot {
    NodeSnapshot::Windows {
        volume_serial: metadata.identity.volume_serial,
        file_id: hex(&metadata.identity.file_id),
        kind: match metadata.kind {
            NodeKind::RegularFile => SnapshotNodeKind::File,
            NodeKind::Directory => SnapshotNodeKind::Directory,
            NodeKind::ReparsePoint => SnapshotNodeKind::ReparsePoint,
        },
        size: metadata.size,
        allocation_size: metadata.allocation_size,
        link_count: metadata.link_count,
        attributes: metadata.attributes,
        reparse_tag: metadata.reparse_tag,
        creation_time: metadata.creation_time,
        last_write_time: metadata.last_write_time,
        change_time: metadata.change_time,
    }
}

fn same_raw_entries(
    left: &[fence_windows::DirectoryEntry],
    right: &[fence_windows::DirectoryEntry],
) -> bool {
    const DIRECTORY_ATTRIBUTE: u32 = 0x10;
    const REPARSE_ATTRIBUTE: u32 = 0x400;
    let mut left = left.to_vec();
    let mut right = right.to_vec();
    left.sort_by(|a, b| a.name.as_encoded_bytes().cmp(b.name.as_encoded_bytes()));
    right.sort_by(|a, b| a.name.as_encoded_bytes().cmp(b.name.as_encoded_bytes()));
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            let left_structural = left.attributes & (DIRECTORY_ATTRIBUTE | REPARSE_ATTRIBUTE);
            let right_structural = right.attributes & (DIRECTORY_ATTRIBUTE | REPARSE_ATTRIBUTE);
            left.name == right.name
                && left.file_id == right.file_id
                && left.reparse_tag == right.reparse_tag
                && left_structural == right_structural
                && (left_structural & DIRECTORY_ATTRIBUTE != 0
                    || (left.size == right.size
                        && left.last_write_time == right.last_write_time
                        && left.change_time == right.change_time))
        })
}

#[derive(Clone, Copy)]
enum ExpectedKind {
    File,
    Directory,
}

fn map_create_windows(error: WindowsError) -> ToolErrorKind {
    let text = error.to_string();
    if text.contains("exist") || text.contains("collision") {
        ToolErrorKind::AlreadyExists
    } else {
        map_windows(error)
    }
}

fn map_windows(error: WindowsError) -> ToolErrorKind {
    match error {
        WindowsError::IdentityChanged => ToolErrorKind::ChangedDuringOperation,
        WindowsError::NotDirectory => ToolErrorKind::WrongNodeKind,
        WindowsError::Path(_) => ToolErrorKind::InvalidPath,
        WindowsError::PrivateDirectoryReparse => ToolErrorKind::LinkOrReparsePoint,
        WindowsError::Io { .. }
        | WindowsError::Malformed(_)
        | WindowsError::TooLarge(_)
        | WindowsError::NativeStatus { .. }
        | WindowsError::NtStatus { .. } => ToolErrorKind::Filesystem,
    }
}

fn map_io(error: std::io::Error) -> ToolErrorKind {
    match error.kind() {
        std::io::ErrorKind::NotFound => ToolErrorKind::NotFound,
        std::io::ErrorKind::AlreadyExists => ToolErrorKind::AlreadyExists,
        std::io::ErrorKind::InvalidInput => ToolErrorKind::InvalidPath,
        _ => ToolErrorKind::Filesystem,
    }
}
