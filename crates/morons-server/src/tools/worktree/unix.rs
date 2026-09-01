use std::{
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    path::Path,
};

use rustix::fs::{
    AtFlags, Dir, Mode, OFlags, RenameFlags, fchmod, fsync, mkdirat, open, openat, renameat,
    renameat_with, unlinkat,
};

use super::{
    MutationKind, NodeSnapshot, RecoveryPlan, RecoveryPlanParts, SearchState, ToolRecoveryOutcome,
    apply_replacements, bounded_match_text, build_read_output, classify_kind, directory_output,
    root_is_directory, same_node_identity, same_published_node, sha256_hex, temporary_name,
};
use crate::tools::{
    DirectoryListEntry, MAX_FILE_BYTES, MAX_SEARCH_BYTES, MAX_SEARCH_FILES, MAX_SEARCH_MATCHES,
    MAX_SEARCH_OUTPUT_BYTES, SearchMatch, TextReplacement, ToolErrorKind, ToolOutput, WorktreePath,
};

pub(super) struct PreparedMutation {
    parent: File,
    target: Option<File>,
    published: bool,
    temporary_name: String,
    temporary_is_directory: bool,
}

impl Drop for PreparedMutation {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        let flags = if self.temporary_is_directory {
            AtFlags::REMOVEDIR
        } else {
            AtFlags::empty()
        };
        let _ = unlinkat(&self.parent, self.temporary_name.as_str(), flags);
        let _ = fsync(&self.parent);
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
    let before = snapshot(&directory)?;
    let mut entries = Vec::new();
    for name in directory_names(&directory)? {
        if cancelled() {
            return Err(ToolErrorKind::Cancelled);
        }
        let child = open_exact_child(&directory, &name, None)?;
        let metadata = child.metadata().map_err(map_io)?;
        let kind = classify_kind(metadata.is_file(), metadata.is_dir())?;
        entries.push(DirectoryListEntry { name, kind });
    }
    if snapshot(&directory)? != before {
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
    let mut file = open_file(root, path)?;
    let before = snapshot(&file)?;
    let bytes = read_bounded(&mut file, cancelled)?;
    if snapshot(&file)? != before {
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
    let parent = open_directory(root, &parent_path)?;
    let parent_snapshot = snapshot(&parent)?;
    let mut target = open_exact_child(&parent, name, Some(ExpectedKind::File))?;
    let before_snapshot = snapshot(&target)?;
    let bytes = read_bounded(&mut target, cancelled)?;
    if snapshot(&target)? != before_snapshot {
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
    let mode = if target.metadata().map_err(map_io)?.mode() & 0o100 != 0 {
        Mode::RUSR | Mode::WUSR | Mode::XUSR
    } else {
        Mode::RUSR | Mode::WUSR
    };
    let mut staged = create_staged_file(&parent, &temporary_name, mode)?;
    staged.write_all(result.as_bytes()).map_err(map_io)?;
    staged.sync_all().map_err(map_io)?;
    let staged_snapshot = snapshot(&staged)?;
    verify_file(&mut staged, &staged_snapshot, &after_digest)?;
    fsync(&parent).map_err(map_errno)?;
    let plan = RecoveryPlan::from_parts(RecoveryPlanParts {
        kind: MutationKind::EditFile,
        path: path.clone(),
        temporary_name: temporary_name.clone(),
        parent: parent_snapshot,
        before: Some(before_snapshot),
        staged: staged_snapshot,
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
            temporary_is_directory: false,
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
    let parent = open_directory(root, &parent_path)?;
    require_name_absent(&parent, name)?;
    if cancelled() {
        return Err(ToolErrorKind::Cancelled);
    }
    let parent_snapshot = snapshot(&parent)?;
    let temporary_name = temporary_name(operation_id);
    let mut staged = create_staged_file(&parent, &temporary_name, Mode::RUSR | Mode::WUSR)?;
    staged.write_all(content.as_bytes()).map_err(map_io)?;
    staged.sync_all().map_err(map_io)?;
    let staged_snapshot = snapshot(&staged)?;
    let digest = sha256_hex(content.as_bytes());
    verify_file(&mut staged, &staged_snapshot, &digest)?;
    fsync(&parent).map_err(map_errno)?;
    let plan = RecoveryPlan::from_parts(RecoveryPlanParts {
        kind: MutationKind::CreateFile,
        path: path.clone(),
        temporary_name: temporary_name.clone(),
        parent: parent_snapshot,
        before: None,
        staged: staged_snapshot,
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
            temporary_is_directory: false,
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
    let parent = open_directory(root, &parent_path)?;
    require_name_absent(&parent, name)?;
    if cancelled() {
        return Err(ToolErrorKind::Cancelled);
    }
    let parent_snapshot = snapshot(&parent)?;
    let temporary_name = temporary_name(operation_id);
    mkdirat(
        &parent,
        temporary_name.as_str(),
        Mode::RUSR | Mode::WUSR | Mode::XUSR,
    )
    .map_err(map_create_errno)?;
    let staged = open_exact_child(&parent, &temporary_name, Some(ExpectedKind::Directory))?;
    let staged_snapshot = snapshot(&staged)?;
    fsync(&staged).map_err(map_errno)?;
    fsync(&parent).map_err(map_errno)?;
    let plan = RecoveryPlan::from_parts(RecoveryPlanParts {
        kind: MutationKind::CreateDirectory,
        path: path.clone(),
        temporary_name: temporary_name.clone(),
        parent: parent_snapshot,
        before: None,
        staged: staged_snapshot,
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
            temporary_is_directory: true,
        },
    ))
}

pub(super) fn publish(
    _root: &Path,
    mut prepared: PreparedMutation,
    plan: &RecoveryPlan,
) -> Result<ToolOutput, ToolErrorKind> {
    if !same_node_identity(&snapshot(&prepared.parent)?, plan.parent()) {
        return Err(ToolErrorKind::ChangedDuringOperation);
    }
    let (_, name) = plan.path().parent_and_name()?;
    match plan.kind() {
        MutationKind::EditFile => {
            let retained = prepared.target.as_mut().ok_or(ToolErrorKind::Filesystem)?;
            if Some(&snapshot(retained)?) != plan.before() {
                return Err(ToolErrorKind::ChangedDuringOperation);
            }
            let mut current = open_exact_child(&prepared.parent, name, Some(ExpectedKind::File))?;
            if Some(&snapshot(&current)?) != plan.before() {
                return Err(ToolErrorKind::ChangedDuringOperation);
            }
            let before_digest = plan.before_sha256().ok_or(ToolErrorKind::Filesystem)?;
            verify_file(
                &mut current,
                plan.before().ok_or(ToolErrorKind::Filesystem)?,
                before_digest,
            )?;
            let staged = open_exact_child(
                &prepared.parent,
                plan.temporary_name(),
                Some(ExpectedKind::File),
            )?;
            if snapshot(&staged)? != *plan.staged() {
                return Err(ToolErrorKind::ChangedDuringOperation);
            }
            renameat(
                &prepared.parent,
                plan.temporary_name(),
                &prepared.parent,
                name,
            )
            .map_err(map_errno)?;
        }
        MutationKind::CreateFile | MutationKind::CreateDirectory => {
            require_name_absent(&prepared.parent, name)?;
            let expected = match plan.kind() {
                MutationKind::CreateFile => ExpectedKind::File,
                MutationKind::CreateDirectory => ExpectedKind::Directory,
                MutationKind::EditFile => return Err(ToolErrorKind::Filesystem),
            };
            let staged = open_exact_child(&prepared.parent, plan.temporary_name(), Some(expected))?;
            if snapshot(&staged)? != *plan.staged() {
                return Err(ToolErrorKind::ChangedDuringOperation);
            }
            renameat_with(
                &prepared.parent,
                plan.temporary_name(),
                &prepared.parent,
                name,
                RenameFlags::NOREPLACE,
            )
            .map_err(map_create_errno)?;
        }
    }
    prepared.published = true;
    fsync(&prepared.parent).map_err(map_errno)?;
    let mut published = open_exact_child(
        &prepared.parent,
        name,
        Some(match plan.kind() {
            MutationKind::EditFile | MutationKind::CreateFile => ExpectedKind::File,
            MutationKind::CreateDirectory => ExpectedKind::Directory,
        }),
    )?;
    if !same_published_node(&snapshot(&published)?, plan.staged()) {
        return Err(ToolErrorKind::ChangedDuringOperation);
    }
    match plan.kind() {
        MutationKind::EditFile => {
            let digest = plan.after_sha256().ok_or(ToolErrorKind::Filesystem)?;
            verify_published_file(&mut published, plan.staged(), digest)?;
            Ok(ToolOutput::FileEdited {
                path: plan.path().clone(),
                sha256: digest.to_owned(),
                bytes: plan.after_bytes(),
            })
        }
        MutationKind::CreateFile => {
            let digest = plan.after_sha256().ok_or(ToolErrorKind::Filesystem)?;
            verify_published_file(&mut published, plan.staged(), digest)?;
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
    let parent = open_directory(root, &parent_path)?;
    if !same_node_identity(&snapshot(&parent)?, plan.parent()) {
        return Ok(ToolRecoveryOutcome::Uncertain);
    }
    let target = open_optional_child(&parent, name)?;
    let temporary = open_optional_child(&parent, plan.temporary_name())?;
    let target_is_after = target.as_ref().is_some_and(|file| {
        snapshot(file)
            .ok()
            .is_some_and(|snapshot| same_published_node(&snapshot, plan.staged()))
    });
    let target_is_before = match (target.as_ref(), plan.before()) {
        (Some(file), Some(before)) => snapshot(file).ok().as_ref() == Some(before),
        (None, None) => true,
        _ => false,
    };
    let temporary_is_staged = temporary
        .as_ref()
        .is_some_and(|file| snapshot(file).ok().as_ref() == Some(plan.staged()));

    if target_is_after && temporary.is_none() {
        if matches!(
            plan.kind(),
            MutationKind::EditFile | MutationKind::CreateFile
        ) {
            let mut target = target.ok_or(ToolErrorKind::Filesystem)?;
            if verify_published_file(
                &mut target,
                plan.staged(),
                plan.after_sha256().ok_or(ToolErrorKind::Filesystem)?,
            )
            .is_err()
            {
                return Ok(ToolRecoveryOutcome::Uncertain);
            }
        }
        return Ok(ToolRecoveryOutcome::Completed);
    }
    if target_is_before && (temporary.is_none() || temporary_is_staged) {
        if temporary_is_staged {
            let flags = if plan.kind() == MutationKind::CreateDirectory {
                AtFlags::REMOVEDIR
            } else {
                AtFlags::empty()
            };
            unlinkat(&parent, plan.temporary_name(), flags).map_err(map_errno)?;
            fsync(&parent).map_err(map_errno)?;
        }
        return Ok(ToolRecoveryOutcome::NotApplied);
    }
    Ok(ToolRecoveryOutcome::Uncertain)
}

fn search_directory<F>(
    directory: &File,
    relative: &WorktreePath,
    query: &str,
    state: &mut SearchState,
    cancelled: &F,
) -> Result<(), ToolErrorKind>
where
    F: Fn() -> bool,
{
    let before = snapshot(directory)?;
    for name in directory_names(directory)? {
        if cancelled() {
            return Err(ToolErrorKind::Cancelled);
        }
        if state.truncated {
            break;
        }
        let child_relative = relative.join_component(&name)?;
        let mut child = open_exact_child(directory, &name, None)?;
        let metadata = child.metadata().map_err(map_io)?;
        if metadata.is_dir() {
            search_directory(&child, &child_relative, query, state, cancelled)?;
        } else if metadata.is_file() {
            search_file(&mut child, child_relative, query, state, cancelled)?;
        } else {
            return Err(ToolErrorKind::WrongNodeKind);
        }
    }
    if snapshot(directory)? != before {
        return Err(ToolErrorKind::ChangedDuringOperation);
    }
    Ok(())
}

fn search_file<F>(
    file: &mut File,
    relative: WorktreePath,
    query: &str,
    state: &mut SearchState,
    cancelled: &F,
) -> Result<(), ToolErrorKind>
where
    F: Fn() -> bool,
{
    let before = snapshot(file)?;
    let size = file.metadata().map_err(map_io)?.len();
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
    let bytes = read_bounded(file, cancelled)?;
    if snapshot(file)? != before {
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

fn open_root(root: &Path) -> Result<File, ToolErrorKind> {
    if !root_is_directory(root) {
        return Err(ToolErrorKind::WrongNodeKind);
    }
    let descriptor = open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(map_errno)?;
    let file = File::from(descriptor);
    if !file.metadata().map_err(map_io)?.is_dir() {
        return Err(ToolErrorKind::WrongNodeKind);
    }
    Ok(file)
}

fn open_directory(root: &Path, path: &WorktreePath) -> Result<File, ToolErrorKind> {
    let mut directory = open_root(root)?;
    for component in path.components() {
        directory = open_exact_child(&directory, component, Some(ExpectedKind::Directory))?;
    }
    Ok(directory)
}

fn open_file(root: &Path, path: &WorktreePath) -> Result<File, ToolErrorKind> {
    let (parent, name) = path.parent_and_name()?;
    let directory = open_directory(root, &parent)?;
    open_exact_child(&directory, name, Some(ExpectedKind::File))
}

fn open_exact_child(
    directory: &File,
    name: &str,
    expected: Option<ExpectedKind>,
) -> Result<File, ToolErrorKind> {
    if !directory_names(directory)?
        .iter()
        .any(|entry| entry == name)
    {
        return Err(ToolErrorKind::NotFound);
    }
    let flags = match expected {
        Some(ExpectedKind::Directory) => OFlags::RDONLY | OFlags::DIRECTORY,
        Some(ExpectedKind::File) | None => OFlags::RDONLY,
    } | OFlags::NOFOLLOW
        | OFlags::CLOEXEC;
    let descriptor = openat(directory, name, flags, Mode::empty()).map_err(map_errno)?;
    let child = File::from(descriptor);
    let metadata = child.metadata().map_err(map_io)?;
    match expected {
        Some(ExpectedKind::File) if !metadata.is_file() => Err(ToolErrorKind::WrongNodeKind),
        Some(ExpectedKind::Directory) if !metadata.is_dir() => Err(ToolErrorKind::WrongNodeKind),
        None if !metadata.is_file() && !metadata.is_dir() => Err(ToolErrorKind::WrongNodeKind),
        _ => Ok(child),
    }
}

fn open_optional_child(directory: &File, name: &str) -> Result<Option<File>, ToolErrorKind> {
    let names = directory_names(directory)?;
    if !names.iter().any(|entry| entry == name) {
        return Ok(None);
    }
    open_exact_child(directory, name, None).map(Some)
}

fn require_name_absent(directory: &File, name: &str) -> Result<(), ToolErrorKind> {
    if directory_names(directory)?
        .iter()
        .any(|entry| entry == name)
    {
        Err(ToolErrorKind::AlreadyExists)
    } else {
        Ok(())
    }
}

fn directory_names(directory: &File) -> Result<Vec<String>, ToolErrorKind> {
    let mut stream = Dir::read_from(directory).map_err(map_errno)?;
    let mut names = Vec::new();
    while let Some(entry) = stream.read() {
        let entry = entry.map_err(map_errno)?;
        let bytes = entry.file_name().to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        let name = std::str::from_utf8(bytes)
            .map_err(|_| ToolErrorKind::InvalidPath)?
            .to_owned();
        WorktreePath::parse(&name, false)?;
        names.push(name);
    }
    names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    Ok(names)
}

fn create_staged_file(parent: &File, name: &str, mode: Mode) -> Result<File, ToolErrorKind> {
    let descriptor = openat(
        parent,
        name,
        OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        mode,
    )
    .map_err(map_create_errno)?;
    let file = File::from(descriptor);
    fchmod(&file, mode).map_err(map_errno)?;
    Ok(file)
}

fn read_bounded<F>(file: &mut File, cancelled: &F) -> Result<Vec<u8>, ToolErrorKind>
where
    F: Fn() -> bool,
{
    let size = file.metadata().map_err(map_io)?.len();
    if size > MAX_FILE_BYTES {
        return Err(ToolErrorKind::ResourceLimit);
    }
    file.seek(SeekFrom::Start(0)).map_err(map_io)?;
    let capacity = usize::try_from(size).map_err(|_| ToolErrorKind::ResourceLimit)?;
    let mut bytes = Vec::with_capacity(capacity);
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
    if bytes.len() as u64 != size {
        return Err(ToolErrorKind::ChangedDuringOperation);
    }
    Ok(bytes)
}

fn verify_file(
    file: &mut File,
    expected: &NodeSnapshot,
    expected_digest: &str,
) -> Result<(), ToolErrorKind> {
    if snapshot(file)? != *expected {
        return Err(ToolErrorKind::ChangedDuringOperation);
    }
    let bytes = read_bounded(file, &|| false)?;
    if snapshot(file)? != *expected || sha256_hex(&bytes) != expected_digest {
        return Err(ToolErrorKind::ChangedDuringOperation);
    }
    Ok(())
}

fn verify_published_file(
    file: &mut File,
    staged: &NodeSnapshot,
    expected_digest: &str,
) -> Result<(), ToolErrorKind> {
    if !same_published_node(&snapshot(file)?, staged) {
        return Err(ToolErrorKind::ChangedDuringOperation);
    }
    let bytes = read_bounded(file, &|| false)?;
    if !same_published_node(&snapshot(file)?, staged) || sha256_hex(&bytes) != expected_digest {
        return Err(ToolErrorKind::ChangedDuringOperation);
    }
    Ok(())
}

fn snapshot(file: &File) -> Result<NodeSnapshot, ToolErrorKind> {
    snapshot_metadata(&file.metadata().map_err(map_io)?)
}

fn snapshot_metadata(metadata: &Metadata) -> Result<NodeSnapshot, ToolErrorKind> {
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(ToolErrorKind::WrongNodeKind);
    }
    Ok(NodeSnapshot::Unix {
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        links: metadata.nlink(),
        size: metadata.size(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    })
}

#[derive(Clone, Copy)]
enum ExpectedKind {
    File,
    Directory,
}

fn map_errno(error: rustix::io::Errno) -> ToolErrorKind {
    map_io(std::io::Error::from_raw_os_error(error.raw_os_error()))
}

fn map_create_errno(error: rustix::io::Errno) -> ToolErrorKind {
    let error = std::io::Error::from_raw_os_error(error.raw_os_error());
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        ToolErrorKind::AlreadyExists
    } else {
        map_io(error)
    }
}

fn map_io(error: std::io::Error) -> ToolErrorKind {
    match error.kind() {
        std::io::ErrorKind::NotFound => ToolErrorKind::NotFound,
        std::io::ErrorKind::AlreadyExists => ToolErrorKind::AlreadyExists,
        std::io::ErrorKind::InvalidInput => ToolErrorKind::InvalidPath,
        _ if error.raw_os_error() == Some(40) => ToolErrorKind::LinkOrReparsePoint,
        _ => ToolErrorKind::Filesystem,
    }
}
