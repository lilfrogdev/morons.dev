use crate::persistence::paths::{StoragePaths, validate_private_directory};
use crate::persistence::{PersistenceError, PersistenceResourceLimit};
use morons_protocol::{DiffChange, DiffChangeKind, DiffNodeKind};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::Path,
    time::{Duration, Instant},
};

#[cfg(windows)]
use fence_windows::{DirectoryEntry, DirectoryHandle, NodeHandle, NodeKind, RootHandle};
#[cfg(unix)]
use std::{fs, os::unix::fs::MetadataExt};

const CONTENT_MANIFEST_CONTEXT: &[u8] = b"morons.dev/repository-manifest/v1\0";
const REVIEW_MANIFEST_CONTEXT: &[u8] = b"morons.dev/review-manifest/v1\0";
const MAX_DEPTH: usize = 64;
const MAX_PATH_BYTES: usize = 4_096;
const MAX_ENTRIES: u64 = 50_000;
const MAX_FILE_BYTES: u64 = 32 * 1_024 * 1_024;
const MAX_TOTAL_BYTES: u64 = 256 * 1_024 * 1_024;
const MAX_EXCERPT_SOURCE_BYTES: u64 = 16 * 1_024;
const MAX_EXCERPT_BYTES: usize = 24 * 1_024;
const MAX_PAGE_EXCERPT_BYTES: usize = 128 * 1_024;
const MAX_PAGE_EXCERPT_LINES: usize = 8_000;
const READ_BUFFER_BYTES: usize = 64 * 1_024;
const REVIEW_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) struct ReviewScan {
    pub changes: Vec<DiffChange>,
    pub active_manifest: [u8; 32],
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Directory,
    File,
}

struct Entry {
    kind: EntryKind,
    digest: Option<[u8; 32]>,
    bytes: Option<u64>,
    executable: bool,
    binary: bool,
    text: Option<String>,
}

struct ScannedTree {
    entries: BTreeMap<String, Entry>,
    content_manifest: [u8; 32],
    review_manifest: [u8; 32],
}

struct ScanState {
    entries: BTreeMap<String, Entry>,
    content_manifest: Sha256,
    review_manifest: Sha256,
    entry_count: u64,
    logical_bytes: u64,
    deadline: Instant,
}

impl ScanState {
    fn new(deadline: Instant) -> Self {
        let mut content_manifest = Sha256::new();
        content_manifest.update(CONTENT_MANIFEST_CONTEXT);
        let mut review_manifest = Sha256::new();
        review_manifest.update(REVIEW_MANIFEST_CONTEXT);
        Self {
            entries: BTreeMap::new(),
            content_manifest,
            review_manifest,
            entry_count: 0,
            logical_bytes: 0,
            deadline,
        }
    }

    fn check_deadline(&self) -> Result<(), PersistenceError> {
        if Instant::now() >= self.deadline {
            Err(limit())
        } else {
            Ok(())
        }
    }

    fn reserve(&mut self, path: &str) -> Result<(), PersistenceError> {
        self.check_deadline()?;
        if path.len() > MAX_PATH_BYTES || self.entry_count >= MAX_ENTRIES {
            return Err(limit());
        }
        self.entry_count = self.entry_count.checked_add(1).ok_or_else(limit)?;
        Ok(())
    }

    fn insert_directory(&mut self, path: String) -> Result<(), PersistenceError> {
        self.reserve(&path)?;
        self.content_manifest.update([0]);
        update_path(&mut self.content_manifest, &path)?;
        self.review_manifest.update([0]);
        update_path(&mut self.review_manifest, &path)?;
        if self
            .entries
            .insert(
                path,
                Entry {
                    kind: EntryKind::Directory,
                    digest: None,
                    bytes: None,
                    executable: false,
                    binary: false,
                    text: None,
                },
            )
            .is_some()
        {
            return Err(invalid());
        }
        Ok(())
    }

    fn insert_file(
        &mut self,
        path: String,
        bytes: u64,
        digest: [u8; 32],
        executable: bool,
        binary: bool,
        text: Option<String>,
    ) -> Result<(), PersistenceError> {
        self.reserve(&path)?;
        self.logical_bytes = self
            .logical_bytes
            .checked_add(bytes)
            .filter(|total| *total <= MAX_TOTAL_BYTES)
            .ok_or_else(limit)?;
        self.content_manifest.update([1]);
        update_path(&mut self.content_manifest, &path)?;
        self.content_manifest.update(bytes.to_be_bytes());
        self.content_manifest.update(digest);
        self.review_manifest.update([1]);
        update_path(&mut self.review_manifest, &path)?;
        self.review_manifest.update(bytes.to_be_bytes());
        self.review_manifest.update(digest);
        self.review_manifest.update([u8::from(executable)]);
        if self
            .entries
            .insert(
                path,
                Entry {
                    kind: EntryKind::File,
                    digest: Some(digest),
                    bytes: Some(bytes),
                    executable,
                    binary,
                    text,
                },
            )
            .is_some()
        {
            return Err(invalid());
        }
        Ok(())
    }
}

impl StoragePaths {
    pub(crate) fn review_diff(
        &self,
        workspace_id: &[u8; 16],
        generation_id: &[u8; 16],
        expected_baseline_manifest: &[u8; 32],
        expected_active_manifest: Option<&[u8; 32]>,
        after: Option<&str>,
        limit: u16,
    ) -> Result<ReviewScan, PersistenceError> {
        let deadline = Instant::now() + REVIEW_TIMEOUT;
        let repository = self.workspace_path(workspace_id).join("repository");
        let baseline_root = repository.join("baseline");
        let active_root = self.worktree_generation_path(workspace_id, generation_id);
        validate_private_directory(&baseline_root)?;
        validate_private_directory(&active_root)?;
        let baseline = scan(&baseline_root, deadline)?;
        if &baseline.content_manifest != expected_baseline_manifest {
            return Err(invalid());
        }
        let active = scan(&active_root, deadline)?;
        if expected_active_manifest.is_some_and(|expected| expected != &active.review_manifest) {
            return Err(PersistenceError::ReviewCursorStale);
        }
        let changes = changes(&baseline.entries, &active.entries, after, limit)?;
        Ok(ReviewScan {
            changes,
            active_manifest: active.review_manifest,
        })
    }
}

fn scan(root: &Path, deadline: Instant) -> Result<ScannedTree, PersistenceError> {
    let mut state = ScanState::new(deadline);
    #[cfg(unix)]
    scan_unix_directory(root, &mut Vec::new(), &mut state)?;
    #[cfg(windows)]
    {
        let root = RootHandle::open(root).map_err(|_| invalid())?;
        scan_windows_directory(root.directory(), &mut Vec::new(), &mut state)?;
    }
    Ok(ScannedTree {
        entries: state.entries,
        content_manifest: state.content_manifest.finalize().into(),
        review_manifest: state.review_manifest.finalize().into(),
    })
}

#[cfg(unix)]
fn scan_unix_directory(
    directory: &Path,
    components: &mut Vec<String>,
    state: &mut ScanState,
) -> Result<(), PersistenceError> {
    state.check_deadline()?;
    if components.len() > MAX_DEPTH {
        return Err(limit());
    }
    let before = fs::symlink_metadata(directory).map_err(|_| invalid())?;
    if !ordinary_directory(&before) {
        return Err(invalid());
    }
    let mut entries = fs::read_dir(directory)
        .map_err(|_| invalid())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid())?;
    entries.sort_by(|left, right| {
        left.file_name()
            .as_encoded_bytes()
            .cmp(right.file_name().as_encoded_bytes())
    });
    for entry in entries {
        state.check_deadline()?;
        let name = entry.file_name().into_string().map_err(|_| invalid())?;
        validate_name(&name, components.len())?;
        components.push(name.clone());
        let relative = components.join("/");
        let path = directory.join(&name);
        let expected = fs::symlink_metadata(&path).map_err(|_| invalid())?;
        if ordinary_directory(&expected) {
            state.insert_directory(relative)?;
            scan_unix_directory(&path, components, state)?;
        } else if ordinary_file(&expected) {
            scan_unix_file(&path, relative, &expected, state)?;
        } else {
            return Err(invalid());
        }
        components.pop();
    }
    let after = fs::symlink_metadata(directory).map_err(|_| invalid())?;
    if !same_unix_identity(&before, &after) || modified(&before) != modified(&after) {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(unix)]
fn scan_unix_file(
    path: &Path,
    relative: String,
    expected: &fs::Metadata,
    state: &mut ScanState,
) -> Result<(), PersistenceError> {
    let size = expected.len();
    if size > MAX_FILE_BYTES {
        return Err(limit());
    }
    let mut file = File::open(path).map_err(|_| invalid())?;
    let opened = file.metadata().map_err(|_| invalid())?;
    if !ordinary_file(&opened) || !same_unix_identity(expected, &opened) || opened.len() != size {
        return Err(invalid());
    }
    let (digest, binary, text) = read_file(&mut file, size, state)?;
    let after_handle = file.metadata().map_err(|_| invalid())?;
    let after_path = fs::symlink_metadata(path).map_err(|_| invalid())?;
    if !same_unix_identity(expected, &after_handle)
        || !same_unix_identity(expected, &after_path)
        || after_handle.len() != size
        || modified(expected) != modified(&after_handle)
    {
        return Err(invalid());
    }
    state.insert_file(
        relative,
        size,
        digest,
        expected.mode() & 0o111 != 0,
        binary,
        text,
    )
}

#[cfg(windows)]
fn scan_windows_directory(
    directory: &DirectoryHandle,
    components: &mut Vec<String>,
    state: &mut ScanState,
) -> Result<(), PersistenceError> {
    state.check_deadline()?;
    if components.len() > MAX_DEPTH {
        return Err(limit());
    }
    let mut entries = directory.entries().map_err(|_| invalid())?;
    entries.sort_by(|left, right| {
        left.name
            .as_encoded_bytes()
            .cmp(right.name.as_encoded_bytes())
    });
    for entry in &entries {
        state.check_deadline()?;
        let name = entry.name.to_str().ok_or_else(invalid)?.to_owned();
        validate_name(&name, components.len())?;
        components.push(name);
        let relative = components.join("/");
        let node = directory.open_child(entry).map_err(|_| invalid())?;
        match node.metadata().kind {
            NodeKind::Directory => {
                state.insert_directory(relative)?;
                let child = node.into_directory().map_err(|_| invalid())?;
                scan_windows_directory(&child, components, state)?;
            }
            NodeKind::RegularFile => scan_windows_file(&node, relative, state)?,
            NodeKind::ReparsePoint => return Err(invalid()),
        }
        components.pop();
    }
    let mut after = directory.entries().map_err(|_| invalid())?;
    after.sort_by(|left, right| {
        left.name
            .as_encoded_bytes()
            .cmp(right.name.as_encoded_bytes())
    });
    if !same_windows_entries(&entries, &after) {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(windows)]
fn scan_windows_file(
    node: &NodeHandle,
    relative: String,
    state: &mut ScanState,
) -> Result<(), PersistenceError> {
    let expected = node.metadata();
    if expected.size > MAX_FILE_BYTES {
        return Err(limit());
    }
    let mut file = node.try_clone_file().map_err(|_| invalid())?;
    let (digest, binary, text) = read_file(&mut file, expected.size, state)?;
    if node.refresh_metadata().map_err(|_| invalid())? != expected
        || node.verify_path_identity().is_err()
    {
        return Err(invalid());
    }
    state.insert_file(relative, expected.size, digest, false, binary, text)
}

fn read_file(
    file: &mut File,
    expected_size: u64,
    state: &ScanState,
) -> Result<([u8; 32], bool, Option<String>), PersistenceError> {
    let mut digest = Sha256::new();
    let mut captured = if expected_size <= MAX_EXCERPT_SOURCE_BYTES {
        Some(Vec::with_capacity(
            usize::try_from(expected_size).map_err(|_| limit())?,
        ))
    } else {
        None
    };
    let mut total = 0_u64;
    let mut buffer = vec![0_u8; READ_BUFFER_BYTES];
    loop {
        state.check_deadline()?;
        let count = file.read(&mut buffer).map_err(|_| invalid())?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(count).map_err(|_| limit())?)
            .filter(|total| *total <= expected_size)
            .ok_or_else(invalid)?;
        digest.update(&buffer[..count]);
        if let Some(captured) = captured.as_mut() {
            captured.extend_from_slice(&buffer[..count]);
        }
    }
    buffer.fill(0);
    if total != expected_size {
        return Err(invalid());
    }
    let (binary, text) = match captured {
        Some(bytes) => match String::from_utf8(bytes) {
            Ok(text) => (false, Some(text)),
            Err(_) => (true, None),
        },
        None => (false, None),
    };
    Ok((digest.finalize().into(), binary, text))
}

fn changes(
    baseline: &BTreeMap<String, Entry>,
    active: &BTreeMap<String, Entry>,
    after: Option<&str>,
    limit: u16,
) -> Result<Vec<DiffChange>, PersistenceError> {
    let paths = baseline
        .keys()
        .chain(active.keys())
        .collect::<BTreeSet<_>>();
    let mut output = Vec::new();
    let mut excerpt_bytes = 0_usize;
    let mut excerpt_lines = 0_usize;
    for path in paths {
        if after.is_some_and(|after| path.as_str() <= after) {
            continue;
        }
        let old = baseline.get(path);
        let new = active.get(path);
        let kind = match (old, new) {
            (None, Some(_)) => DiffChangeKind::Added,
            (Some(_), None) => DiffChangeKind::Deleted,
            (Some(old), Some(new)) if old.kind != new.kind || old.digest != new.digest => {
                DiffChangeKind::Modified
            }
            (Some(old), Some(new))
                if old.kind == EntryKind::File && old.executable != new.executable =>
            {
                DiffChangeKind::ModeChanged
            }
            _ => continue,
        };
        let binary = [old, new]
            .into_iter()
            .flatten()
            .any(|entry| entry.kind == EntryKind::File && entry.binary);
        let mut excerpt = if binary
            || old.is_some_and(|entry| entry.kind == EntryKind::Directory)
            || new.is_some_and(|entry| entry.kind == EntryKind::Directory)
        {
            None
        } else {
            diff_excerpt(path, kind, old, new)
        };
        if let Some(value) = excerpt.as_ref() {
            let lines = value.lines().count();
            if excerpt_bytes
                .checked_add(value.len())
                .is_none_or(|bytes| bytes > MAX_PAGE_EXCERPT_BYTES)
                || excerpt_lines
                    .checked_add(lines)
                    .is_none_or(|count| count > MAX_PAGE_EXCERPT_LINES)
            {
                excerpt = None;
            } else {
                excerpt_bytes += value.len();
                excerpt_lines += lines;
            }
        }
        output.push(DiffChange {
            path: path.clone(),
            kind,
            old_kind: old.map(protocol_kind),
            new_kind: new.map(protocol_kind),
            old_sha256: old.and_then(|entry| entry.digest.as_ref()).map(hex),
            new_sha256: new.and_then(|entry| entry.digest.as_ref()).map(hex),
            old_bytes: old.and_then(|entry| entry.bytes),
            new_bytes: new.and_then(|entry| entry.bytes),
            binary,
            excerpt,
        });
        if output.len() >= usize::from(limit) {
            break;
        }
    }
    Ok(output)
}

fn diff_excerpt(
    path: &str,
    kind: DiffChangeKind,
    old: Option<&Entry>,
    new: Option<&Entry>,
) -> Option<String> {
    let mut value = format!("--- a/{path}\n+++ b/{path}\n");
    match kind {
        DiffChangeKind::Added => {
            let new = new?.text.as_ref()?;
            value.push_str(&format!("@@ -0,0 +1,{} @@\n", new.lines().count()));
            append_lines(&mut value, '+', new);
        }
        DiffChangeKind::Deleted => {
            let old = old?.text.as_ref()?;
            value.push_str(&format!("@@ -1,{} +0,0 @@\n", old.lines().count()));
            append_lines(&mut value, '-', old);
        }
        DiffChangeKind::Modified => {
            let old = old?.text.as_ref()?;
            let new = new?.text.as_ref()?;
            value.push_str(&format!(
                "@@ -1,{} +1,{} @@\n",
                old.lines().count(),
                new.lines().count()
            ));
            append_lines(&mut value, '-', old);
            append_lines(&mut value, '+', new);
        }
        DiffChangeKind::ModeChanged => value.push_str("executable mode changed\n"),
    }
    let sanitized: String = value
        .chars()
        .filter(|character| {
            *character == '\n'
                || *character == '\t'
                || !character.is_control()
                    && !matches!(
                        character,
                        '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}'
                            | '\u{2066}'..='\u{2069}'
                    )
        })
        .collect();
    Some(truncate_utf8(sanitized, MAX_EXCERPT_BYTES))
}

fn append_lines(value: &mut String, prefix: char, text: &str) {
    for line in text.lines() {
        value.push(prefix);
        value.push_str(line);
        value.push('\n');
    }
}

fn truncate_utf8(mut value: String, maximum: usize) -> String {
    if value.len() <= maximum {
        return value;
    }
    let mut boundary = maximum;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value
}

fn protocol_kind(entry: &Entry) -> DiffNodeKind {
    match entry.kind {
        EntryKind::Directory => DiffNodeKind::Directory,
        EntryKind::File => DiffNodeKind::File,
    }
}

fn update_path(manifest: &mut Sha256, path: &str) -> Result<(), PersistenceError> {
    let length = u32::try_from(path.len()).map_err(|_| limit())?;
    manifest.update(length.to_be_bytes());
    manifest.update(path.as_bytes());
    Ok(())
}

fn validate_name(name: &str, depth: usize) -> Result<(), PersistenceError> {
    if name.is_empty() || name == "." || name == ".." || name.eq_ignore_ascii_case(".git") {
        return Err(invalid());
    }
    if depth >= MAX_DEPTH || name.chars().any(char::is_control) {
        return Err(limit());
    }
    Ok(())
}

#[cfg(unix)]
fn ordinary_file(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_file()
}

#[cfg(unix)]
fn ordinary_directory(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_dir() && !metadata.file_type().is_symlink()
}

#[cfg(unix)]
fn same_unix_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino() && left.file_type() == right.file_type()
}

#[cfg(unix)]
fn modified(metadata: &fs::Metadata) -> Option<std::time::SystemTime> {
    metadata.modified().ok()
}

#[cfg(windows)]
fn same_windows_entries(left: &[DirectoryEntry], right: &[DirectoryEntry]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left.name == right.name
                && left.file_id == right.file_id
                && left.attributes == right.attributes
                && left.reparse_tag == right.reparse_tag
                && left.size == right.size
                && left.last_write_time == right.last_write_time
                && left.change_time == right.change_time
        })
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn invalid() -> PersistenceError {
    PersistenceError::InvalidState {
        reason: "a reviewed tree is invalid or changed during traversal",
    }
}

fn limit() -> PersistenceError {
    PersistenceError::ResourceLimit {
        resource: PersistenceResourceLimit::Workspace,
    }
}
