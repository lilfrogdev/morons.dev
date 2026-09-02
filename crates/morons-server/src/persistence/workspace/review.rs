use super::repository_import::copy_command_tree;
use crate::persistence::paths::{
    StoragePaths, encode_hex, ensure_private_directory, path_entry_exists, sync_directory,
    validate_private_directory,
};
use crate::persistence::{PersistenceError, PersistenceResourceLimit};
use morons_protocol::{DiffChange, DiffChangeKind};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

const MAX_ENTRIES: usize = 50_000;
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const MAX_EXCERPT_BYTES: usize = 64 * 1024;

struct Node {
    digest: [u8; 32],
    bytes: u64,
    executable: bool,
    text: Option<String>,
}

impl StoragePaths {
    pub(crate) fn export_worktree(
        &self,
        workspace_id: &[u8; 16],
        generation_id: &[u8; 16],
        operation_id: &[u8; 16],
        destination: &str,
    ) -> Result<crate::persistence::RepositoryImportOutcome, PersistenceError> {
        let destination = PathBuf::from(destination);
        if !destination.is_absolute() || path_entry_exists(&destination)? {
            return Err(PersistenceError::InvalidInput {
                reason: "export destination must be absent and absolute",
            });
        }
        let parent = destination.parent().ok_or_else(invalid)?;
        let parent_metadata = fs::symlink_metadata(parent)?;
        if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
            return Err(PersistenceError::InvalidInput {
                reason: "export parent is invalid",
            });
        }
        let application = fs::canonicalize(self.application_directory())?;
        let parent_canonical = fs::canonicalize(parent)?;
        if parent_canonical.starts_with(&application) || application.starts_with(&parent_canonical)
        {
            return Err(PersistenceError::InvalidInput {
                reason: "export destination overlaps protected Morons state",
            });
        }
        let staging = parent.join(format!(".morons-export-{}", encode_hex(operation_id)));
        let mirror = parent.join(format!(
            ".morons-export-mirror-{}",
            encode_hex(operation_id)
        ));
        if path_entry_exists(&staging)? || path_entry_exists(&mirror)? {
            return Err(invalid());
        }
        ensure_private_directory(&staging)?;
        ensure_private_directory(&mirror)?;
        let active = self.worktree_generation_path(workspace_id, generation_id);
        let outcome = match copy_command_tree(&active, &mirror, &staging) {
            Ok(outcome) => outcome,
            Err(error) => {
                let _ = fs::remove_dir_all(&staging);
                let _ = fs::remove_dir_all(&mirror);
                return Err(error);
            }
        };
        sync_directory(&staging)?;
        fs::rename(&staging, &destination)?;
        sync_directory(parent)?;
        Ok(outcome)
    }

    pub(crate) fn review_diff(
        &self,
        workspace_id: &[u8; 16],
        generation_id: &[u8; 16],
        after: Option<&str>,
        limit: u16,
    ) -> Result<Vec<DiffChange>, PersistenceError> {
        let repository = self.workspace_path(workspace_id).join("repository");
        let baseline = repository.join("baseline");
        let active = self.worktree_generation_path(workspace_id, generation_id);
        validate_private_directory(&baseline)?;
        validate_private_directory(&active)?;
        let baseline = scan(&baseline)?;
        let active = scan(&active)?;
        let paths = baseline
            .keys()
            .chain(active.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut changes = Vec::new();
        for path in paths {
            if after.is_some_and(|after| path.as_str() <= after) {
                continue;
            }
            let old = baseline.get(&path);
            let new = active.get(&path);
            let kind = match (old, new) {
                (None, Some(_)) => DiffChangeKind::Added,
                (Some(_), None) => DiffChangeKind::Deleted,
                (Some(old), Some(new)) if old.digest != new.digest => DiffChangeKind::Modified,
                (Some(old), Some(new)) if old.executable != new.executable => {
                    DiffChangeKind::ModeChanged
                }
                _ => continue,
            };
            let binary = old.and_then(|node| node.text.as_ref()).is_none() && old.is_some()
                || new.and_then(|node| node.text.as_ref()).is_none() && new.is_some();
            let excerpt = if binary {
                None
            } else {
                diff_excerpt(&path, kind, old, new)
            };
            changes.push(DiffChange {
                path,
                kind,
                old_sha256: old.map(|node| hex(&node.digest)),
                new_sha256: new.map(|node| hex(&node.digest)),
                old_bytes: old.map(|node| node.bytes),
                new_bytes: new.map(|node| node.bytes),
                binary,
                excerpt,
            });
            if changes.len() >= usize::from(limit) {
                break;
            }
        }
        Ok(changes)
    }
}

fn scan(root: &Path) -> Result<BTreeMap<String, Node>, PersistenceError> {
    let mut output = BTreeMap::new();
    let mut total = 0_u64;
    scan_dir(root, root, &mut output, &mut total)?;
    Ok(output)
}

fn scan_dir(
    root: &Path,
    directory: &Path,
    output: &mut BTreeMap<String, Node>,
    total: &mut u64,
) -> Result<(), PersistenceError> {
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(invalid());
    }
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        if output.len() >= MAX_ENTRIES {
            return Err(limit());
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(invalid());
        }
        if metadata.is_dir() {
            scan_dir(root, &path, output, total)?;
            continue;
        }
        if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
            return Err(invalid());
        }
        *total = total
            .checked_add(metadata.len())
            .filter(|total| *total <= MAX_TOTAL_BYTES)
            .ok_or_else(limit)?;
        let relative = path.strip_prefix(root).map_err(|_| invalid())?;
        let relative = relative
            .components()
            .map(|part| part.as_os_str().to_str().ok_or_else(invalid))
            .collect::<Result<Vec<_>, _>>()?
            .join("/");
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).map_err(|_| limit())?);
        File::open(&path)?.read_to_end(&mut bytes)?;
        let digest = Sha256::digest(&bytes).into();
        let text = if bytes.len() <= MAX_EXCERPT_BYTES {
            String::from_utf8(bytes).ok()
        } else {
            None
        };
        #[cfg(unix)]
        let executable = metadata.permissions().mode() & 0o111 != 0;
        #[cfg(not(unix))]
        let executable = false;
        output.insert(
            relative,
            Node {
                digest,
                bytes: metadata.len(),
                executable,
                text,
            },
        );
    }
    Ok(())
}

fn diff_excerpt(
    path: &str,
    kind: DiffChangeKind,
    old: Option<&Node>,
    new: Option<&Node>,
) -> Option<String> {
    let mut value = format!("--- a/{path}\n+++ b/{path}\n");
    match kind {
        DiffChangeKind::Added => {
            for line in new?.text.as_ref()?.lines() {
                value.push('+');
                value.push_str(line);
                value.push('\n');
            }
        }
        DiffChangeKind::Deleted => {
            for line in old?.text.as_ref()?.lines() {
                value.push('-');
                value.push_str(line);
                value.push('\n');
            }
        }
        DiffChangeKind::Modified => {
            for line in old?.text.as_ref()?.lines() {
                value.push('-');
                value.push_str(line);
                value.push('\n');
            }
            for line in new?.text.as_ref()?.lines() {
                value.push('+');
                value.push_str(line);
                value.push('\n');
            }
        }
        DiffChangeKind::ModeChanged => value.push_str("executable mode changed\n"),
    }
    if value.len() > MAX_EXCERPT_BYTES {
        let mut boundary = MAX_EXCERPT_BYTES;
        while !value.is_char_boundary(boundary) {
            boundary -= 1;
        }
        value.truncate(boundary);
    }
    Some(value)
}
fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn invalid() -> PersistenceError {
    PersistenceError::InvalidState {
        reason: "review tree is invalid",
    }
}
fn limit() -> PersistenceError {
    PersistenceError::ResourceLimit {
        resource: PersistenceResourceLimit::Workspace,
    }
}
