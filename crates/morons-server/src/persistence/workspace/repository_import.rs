use std::{
    ffi::OsStr,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[cfg(windows)]
use fence_windows::{DirectoryEntry, DirectoryHandle, NodeHandle, NodeKind, RootHandle};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

use sha2::{Digest, Sha256};

use crate::persistence::{
    PersistenceError, PersistenceResourceLimit, RepositoryImportOutcome, RepositoryImportPlan,
    WorktreeLayoutPlan,
};

use super::{StoragePaths, validate_workspace_identity};
use crate::persistence::paths::{
    create_private_file, encode_hex, ensure_private_directory, path_entry_exists, sync_directory,
    validate_private_directory, validate_private_file,
};

const REPOSITORY_DIRECTORY_NAME: &str = "repository";
const BASELINE_DIRECTORY_NAME: &str = "baseline";
const LEGACY_WORKTREE_DIRECTORY_NAME: &str = "worktree";
const GENERATIONS_DIRECTORY_NAME: &str = "generations";
const GENERATION_CONTENT_DIRECTORY_NAME: &str = "content";
const GENERATION_METADATA_FILE_NAME: &str = "generation-metadata";
const IMPORT_METADATA_FILE_NAME: &str = "import-metadata";
const IMPORT_METADATA_UPGRADE_FILE_NAME: &str = ".import-metadata-upgrading";
const STAGING_PREFIX: &str = ".repository-importing-";
const METADATA_CONTEXT: &[u8] = b"morons.dev/repository-import/v2\0";
const LEGACY_METADATA_CONTEXT: &[u8] = b"morons.dev/repository-import/v1\0";
const GENERATION_METADATA_CONTEXT: &[u8] = b"morons.dev/worktree-generation/v1\0";
const MANIFEST_CONTEXT: &[u8] = b"morons.dev/repository-manifest/v1\0";
const MAX_PATH_DEPTH: usize = 64;
const MAX_RELATIVE_PATH_BYTES: usize = 4096;
const MAX_ENTRIES: u64 = 50_000;
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_LOGICAL_BYTES: u64 = 256 * 1024 * 1024;
const COPY_BUFFER_BYTES: usize = 64 * 1024;
#[cfg(windows)]
const WINDOWS_DIRECTORY_ATTRIBUTE: u32 = 0x10;
#[cfg(windows)]
const WINDOWS_REPARSE_ATTRIBUTE: u32 = 0x400;
#[cfg(windows)]
const WINDOWS_STRUCTURAL_ATTRIBUTES: u32 = WINDOWS_DIRECTORY_ATTRIBUTE | WINDOWS_REPARSE_ATTRIBUTE;
const METADATA_BYTES: usize = METADATA_CONTEXT.len() + 16 + 16 + 16 + 8 + 8 + 8 + 32;
const LEGACY_METADATA_BYTES: usize = LEGACY_METADATA_CONTEXT.len() + 16 + 16 + 8 + 8 + 8 + 32;
const GENERATION_METADATA_BYTES: usize =
    GENERATION_METADATA_CONTEXT.len() + 16 + 16 + 8 + 8 + 8 + 32;

pub(crate) enum RepositoryRecovery {
    Complete(RepositoryImportOutcome),
    NotApplied,
    Blocked,
}

pub(crate) enum WorktreeLayoutRecovery {
    Complete(RepositoryImportOutcome),
    Blocked,
}

impl StoragePaths {
    pub(crate) fn migrate_worktree_layout(
        &self,
        plan: WorktreeLayoutPlan,
    ) -> Result<WorktreeLayoutRecovery, PersistenceError> {
        let repository = self
            .workspace_path(&plan.workspace_id)
            .join(REPOSITORY_DIRECTORY_NAME);
        validate_private_directory(&repository)?;
        let legacy = repository.join(LEGACY_WORKTREE_DIRECTORY_NAME);
        let generations = repository.join(GENERATIONS_DIRECTORY_NAME);
        let generation = generations.join(encode_hex(&plan.generation_id));
        let content = generation.join(GENERATION_CONTENT_DIRECTORY_NAME);
        let final_exists = path_entry_exists(&content)?;
        let legacy_exists = path_entry_exists(&legacy)?;
        if final_exists {
            validate_private_directory(&generations)?;
            validate_private_directory(&generation)?;
            validate_private_directory(&content)?;
            let outcome = scan_baseline_tree(&content)?;
            let metadata = generation.join(GENERATION_METADATA_FILE_NAME);
            if path_entry_exists(&metadata)? {
                if read_generation_metadata(&generation, &plan.workspace_id, &plan.generation_id)?
                    != outcome
                {
                    return Ok(WorktreeLayoutRecovery::Blocked);
                }
            } else {
                write_generation_metadata(
                    &generation,
                    &plan.workspace_id,
                    &plan.generation_id,
                    outcome,
                )?;
                sync_directory(&generation)?;
            }
            upgrade_import_metadata(&repository, &plan, outcome)?;
            return Ok(WorktreeLayoutRecovery::Complete(outcome));
        }
        if !legacy_exists || path_entry_exists(&generations)? {
            return Ok(WorktreeLayoutRecovery::Blocked);
        }
        validate_private_directory(&legacy)?;
        ensure_private_directory(&generations)?;
        ensure_private_directory(&generation)?;
        fs::rename(&legacy, &content)?;
        sync_directory(&generation)?;
        let outcome = scan_baseline_tree(&content)?;
        write_generation_metadata(
            &generation,
            &plan.workspace_id,
            &plan.generation_id,
            outcome,
        )?;
        sync_directory(&generation)?;
        sync_directory(&generations)?;
        upgrade_import_metadata(&repository, &plan, outcome)?;
        sync_directory(&repository)?;
        Ok(WorktreeLayoutRecovery::Complete(outcome))
    }

    pub(crate) fn cleanup_legacy_worktree(
        &self,
        workspace_id: &[u8; 16],
    ) -> Result<(), PersistenceError> {
        let repository = self
            .workspace_path(workspace_id)
            .join(REPOSITORY_DIRECTORY_NAME);
        let legacy = repository.join(LEGACY_WORKTREE_DIRECTORY_NAME);
        if path_entry_exists(&legacy)? {
            validate_private_directory(&legacy)?;
            fs::remove_dir_all(&legacy)?;
            sync_directory(&repository)?;
        }
        Ok(())
    }

    pub(crate) fn import_repository(
        &self,
        plan: RepositoryImportPlan,
        source_path: &str,
    ) -> Result<RepositoryImportOutcome, PersistenceError> {
        let source = validate_source_root(self, Path::new(source_path))?;
        let workspace = self.workspace_path(&plan.workspace_id);
        validate_private_directory(&workspace)?;
        validate_workspace_identity(&workspace.join("identity"), &plan.workspace_id)?;
        let final_path = workspace.join(REPOSITORY_DIRECTORY_NAME);
        let staging = workspace.join(staging_name(&plan.operation_id));
        if path_entry_exists(&final_path)? || path_entry_exists(&staging)? {
            return Err(PersistenceError::WorkspaceBlocked);
        }

        ensure_private_directory(&staging)?;
        let baseline = staging.join(BASELINE_DIRECTORY_NAME);
        let generations = staging.join(GENERATIONS_DIRECTORY_NAME);
        let generation = generations.join(encode_hex(&plan.generation_id));
        let worktree = generation.join(GENERATION_CONTENT_DIRECTORY_NAME);
        ensure_private_directory(&baseline)?;
        ensure_private_directory(&generations)?;
        ensure_private_directory(&generation)?;
        ensure_private_directory(&worktree)?;
        let result = copy_repository_tree(&source, &baseline, &worktree).and_then(|outcome| {
            write_generation_metadata(
                &generation,
                &plan.workspace_id,
                &plan.generation_id,
                outcome,
            )?;
            sync_directory(&generation)?;
            sync_directory(&generations)?;
            write_import_metadata(&staging, &plan, outcome)?;
            sync_directory(&staging)?;
            fs::rename(&staging, &final_path)?;
            sync_directory(&workspace)?;
            Ok(outcome)
        });
        if result.is_err() {
            if path_entry_exists(&final_path).unwrap_or(true) {
                return Err(PersistenceError::WorkspaceBlocked);
            }
            if path_entry_exists(&staging).unwrap_or(true)
                && remove_confined_tree(&workspace, &staging).is_err()
            {
                return Err(PersistenceError::WorkspaceBlocked);
            }
        }
        result
    }

    pub(crate) fn validate_completed_repository(
        &self,
        plan: RepositoryImportPlan,
        expected: RepositoryImportOutcome,
    ) -> Result<(), PersistenceError> {
        let workspace = self.workspace_path(&plan.workspace_id);
        validate_private_directory(&workspace)?;
        validate_workspace_identity(&workspace.join("identity"), &plan.workspace_id)?;
        let repository = workspace.join(REPOSITORY_DIRECTORY_NAME);
        validate_private_directory(&repository)?;
        let metadata = read_import_metadata(&repository, &plan)?;
        let baseline = repository.join(BASELINE_DIRECTORY_NAME);
        validate_private_directory(&baseline)?;
        let actual = scan_baseline_tree(&baseline)?;
        if metadata != expected || actual != expected {
            return Err(invalid_repository());
        }
        Ok(())
    }

    pub(crate) fn recover_repository_import(
        &self,
        plan: RepositoryImportPlan,
    ) -> Result<RepositoryRecovery, PersistenceError> {
        let workspace = self.workspace_path(&plan.workspace_id);
        validate_private_directory(&workspace)?;
        validate_workspace_identity(&workspace.join("identity"), &plan.workspace_id)?;
        let final_path = workspace.join(REPOSITORY_DIRECTORY_NAME);
        let staging = workspace.join(staging_name(&plan.operation_id));
        let final_exists = path_entry_exists(&final_path)?;
        let staging_exists = path_entry_exists(&staging)?;
        match (final_exists, staging_exists) {
            (true, true) => Ok(RepositoryRecovery::Blocked),
            (true, false) => validate_repository_directory(&final_path, &plan)
                .map(RepositoryRecovery::Complete)
                .or(Ok(RepositoryRecovery::Blocked)),
            (false, true) => {
                let metadata_path = staging.join(IMPORT_METADATA_FILE_NAME);
                if !path_entry_exists(&metadata_path)? {
                    remove_confined_tree(&workspace, &staging)?;
                    return Ok(RepositoryRecovery::NotApplied);
                }
                let outcome = match validate_repository_directory(&staging, &plan) {
                    Ok(outcome) => outcome,
                    Err(_) => return Ok(RepositoryRecovery::Blocked),
                };
                fs::rename(&staging, &final_path)?;
                sync_directory(&workspace)?;
                Ok(RepositoryRecovery::Complete(outcome))
            }
            (false, false) => Ok(RepositoryRecovery::NotApplied),
        }
    }
}

fn validate_source_root(paths: &StoragePaths, source: &Path) -> Result<PathBuf, PersistenceError> {
    let metadata = fs::symlink_metadata(source).map_err(|_| invalid_source())?;
    if !ordinary_directory(&metadata) {
        return Err(invalid_source());
    }
    let source = fs::canonicalize(source).map_err(|_| invalid_source())?;
    let application = fs::canonicalize(paths.application_directory()).map_err(|_| {
        PersistenceError::InvalidState {
            reason: "the application root could not be resolved",
        }
    })?;
    if source.starts_with(&application) || application.starts_with(&source) {
        return Err(PersistenceError::InvalidInput {
            reason: "a repository source overlaps protected Morons state",
        });
    }
    Ok(source)
}

#[cfg(unix)]
fn copy_repository_tree(
    source: &Path,
    baseline: &Path,
    worktree: &Path,
) -> Result<RepositoryImportOutcome, PersistenceError> {
    let mut state = ImportState {
        manifest: Sha256::new(),
        file_count: 0,
        directory_count: 0,
        logical_bytes: 0,
    };
    state.manifest.update(MANIFEST_CONTEXT);
    copy_directory(source, baseline, worktree, &mut Vec::new(), &mut state)?;
    sync_directory(baseline)?;
    sync_directory(worktree)?;
    Ok(RepositoryImportOutcome {
        file_count: state.file_count,
        directory_count: state.directory_count,
        logical_bytes: state.logical_bytes,
        manifest_digest: state.manifest.finalize().into(),
    })
}

#[cfg(unix)]
fn copy_directory(
    source: &Path,
    baseline: &Path,
    worktree: &Path,
    components: &mut Vec<String>,
    state: &mut ImportState,
) -> Result<(), PersistenceError> {
    let before = fs::symlink_metadata(source).map_err(|_| invalid_source())?;
    if !ordinary_directory(&before) {
        return Err(invalid_source());
    }
    let mut entries = fs::read_dir(source)
        .map_err(|_| invalid_source())?
        .map(|entry| entry.map_err(|_| invalid_source()))
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by(|left, right| {
        left.file_name()
            .as_encoded_bytes()
            .cmp(right.file_name().as_encoded_bytes())
    });

    for entry in entries {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid_source())?;
        if name.eq_ignore_ascii_case(".git") {
            continue;
        }
        validate_component(&name, components)?;
        components.push(name.clone());
        reserve_entry(state)?;
        let relative = relative_path_bytes(components)?;
        let source_path = source.join(&name);
        let baseline_path = baseline.join(&name);
        let worktree_path = worktree.join(&name);
        let metadata = fs::symlink_metadata(&source_path).map_err(|_| invalid_source())?;
        if ordinary_directory(&metadata) {
            state.directory_count = state.directory_count.checked_add(1).ok_or_else(limit)?;
            update_directory_manifest(&mut state.manifest, &relative)?;
            if path_entry_exists(&baseline_path)? || path_entry_exists(&worktree_path)? {
                return Err(invalid_source());
            }
            ensure_private_directory(&baseline_path)?;
            ensure_private_directory(&worktree_path)?;
            copy_directory(
                &source_path,
                &baseline_path,
                &worktree_path,
                components,
                state,
            )?;
            sync_directory(&baseline_path)?;
            sync_directory(&worktree_path)?;
        } else if ordinary_file(&metadata) {
            copy_file(
                &source_path,
                &baseline_path,
                &worktree_path,
                &relative,
                &metadata,
                state,
            )?;
        } else {
            return Err(invalid_source());
        }
        components.pop();
    }
    let after = fs::symlink_metadata(source).map_err(|_| invalid_source())?;
    if !same_identity(&before, &after) {
        return Err(invalid_source());
    }
    Ok(())
}

#[cfg(unix)]
fn copy_file(
    source: &Path,
    baseline: &Path,
    worktree: &Path,
    relative: &[u8],
    expected: &fs::Metadata,
    state: &mut ImportState,
) -> Result<(), PersistenceError> {
    let size = expected.len();
    if size > MAX_FILE_BYTES
        || state
            .logical_bytes
            .checked_add(size)
            .is_none_or(|total| total > MAX_LOGICAL_BYTES)
    {
        return Err(limit());
    }
    let mut source_file = File::open(source).map_err(|_| invalid_source())?;
    let opened = source_file.metadata().map_err(|_| invalid_source())?;
    if !ordinary_file(&opened) || !same_identity(expected, &opened) || opened.len() != size {
        return Err(invalid_source());
    }
    let mut baseline_file = create_private_file(baseline)?;
    let mut worktree_file = create_private_file(worktree)?;
    let mut content = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    loop {
        let read = source_file
            .read(&mut buffer)
            .map_err(|_| invalid_source())?;
        if read == 0 {
            break;
        }
        copied = copied.checked_add(read as u64).ok_or_else(limit)?;
        if copied > size {
            return Err(invalid_source());
        }
        content.update(&buffer[..read]);
        baseline_file.write_all(&buffer[..read])?;
        worktree_file.write_all(&buffer[..read])?;
    }
    buffer.fill(0);
    if copied != size {
        return Err(invalid_source());
    }
    let after_handle = source_file.metadata().map_err(|_| invalid_source())?;
    let after_path = fs::symlink_metadata(source).map_err(|_| invalid_source())?;
    if !same_identity(expected, &after_handle)
        || !same_identity(expected, &after_path)
        || after_handle.len() != size
        || modified_value(expected) != modified_value(&after_handle)
    {
        return Err(invalid_source());
    }
    baseline_file.sync_all()?;
    worktree_file.sync_all()?;
    drop(baseline_file);
    drop(worktree_file);
    #[cfg(unix)]
    if expected.mode() & 0o100 != 0 {
        fs::set_permissions(baseline, fs::Permissions::from_mode(0o700))?;
        File::open(baseline)?.sync_all()?;
        fs::set_permissions(worktree, fs::Permissions::from_mode(0o700))?;
        File::open(worktree)?.sync_all()?;
    }
    let digest: [u8; 32] = content.finalize().into();
    update_file_manifest(&mut state.manifest, relative, size, &digest)?;
    state.file_count += 1;
    state.logical_bytes += size;
    Ok(())
}

#[cfg(windows)]
fn copy_repository_tree(
    source: &Path,
    baseline: &Path,
    worktree: &Path,
) -> Result<RepositoryImportOutcome, PersistenceError> {
    let root = RootHandle::open(source).map_err(|_| invalid_source())?;
    let mut state = ImportState {
        manifest: Sha256::new(),
        file_count: 0,
        directory_count: 0,
        logical_bytes: 0,
    };
    state.manifest.update(MANIFEST_CONTEXT);
    copy_windows_directory(
        root.directory(),
        baseline,
        worktree,
        &mut Vec::new(),
        &mut state,
    )?;
    sync_directory(baseline)?;
    sync_directory(worktree)?;
    Ok(RepositoryImportOutcome {
        file_count: state.file_count,
        directory_count: state.directory_count,
        logical_bytes: state.logical_bytes,
        manifest_digest: state.manifest.finalize().into(),
    })
}

#[cfg(windows)]
fn copy_windows_directory(
    source: &DirectoryHandle,
    baseline: &Path,
    worktree: &Path,
    components: &mut Vec<String>,
    state: &mut ImportState,
) -> Result<(), PersistenceError> {
    let mut entries = source.entries().map_err(|_| invalid_source())?;
    entries.sort_by(|left, right| {
        left.name
            .as_encoded_bytes()
            .cmp(right.name.as_encoded_bytes())
    });
    for entry in &entries {
        let name = entry.name.to_str().ok_or_else(invalid_source)?.to_owned();
        if name.eq_ignore_ascii_case(".git") {
            continue;
        }
        validate_component(&name, components)?;
        components.push(name.clone());
        reserve_entry(state)?;
        let relative = relative_path_bytes(components)?;
        let baseline_path = baseline.join(&name);
        let worktree_path = worktree.join(&name);
        let node = source.open_child(entry).map_err(|_| invalid_source())?;
        match node.metadata().kind {
            NodeKind::Directory => {
                state.directory_count = state.directory_count.checked_add(1).ok_or_else(limit)?;
                update_directory_manifest(&mut state.manifest, &relative)?;
                if path_entry_exists(&baseline_path)? || path_entry_exists(&worktree_path)? {
                    return Err(invalid_source());
                }
                ensure_private_directory(&baseline_path)?;
                ensure_private_directory(&worktree_path)?;
                let directory = node.into_directory().map_err(|_| invalid_source())?;
                copy_windows_directory(
                    &directory,
                    &baseline_path,
                    &worktree_path,
                    components,
                    state,
                )?;
                sync_directory(&baseline_path)?;
                sync_directory(&worktree_path)?;
            }
            NodeKind::RegularFile => {
                copy_windows_file(&node, &baseline_path, &worktree_path, &relative, state)?
            }
            NodeKind::ReparsePoint => return Err(invalid_source()),
        }
        components.pop();
    }
    let mut after = source.entries().map_err(|_| invalid_source())?;
    after.sort_by(|left, right| {
        left.name
            .as_encoded_bytes()
            .cmp(right.name.as_encoded_bytes())
    });
    if !same_windows_directory_entries(&entries, &after) {
        return Err(invalid_source());
    }
    Ok(())
}

#[cfg(windows)]
fn copy_windows_file(
    source: &NodeHandle,
    baseline: &Path,
    worktree: &Path,
    relative: &[u8],
    state: &mut ImportState,
) -> Result<(), PersistenceError> {
    let expected = source.metadata();
    let size = expected.size;
    if size > MAX_FILE_BYTES
        || state
            .logical_bytes
            .checked_add(size)
            .is_none_or(|total| total > MAX_LOGICAL_BYTES)
    {
        return Err(limit());
    }
    let mut source_file = source.try_clone_file().map_err(|_| invalid_source())?;
    let mut baseline_file = create_private_file(baseline)?;
    let mut worktree_file = create_private_file(worktree)?;
    let mut content = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    loop {
        let read = source_file
            .read(&mut buffer)
            .map_err(|_| invalid_source())?;
        if read == 0 {
            break;
        }
        copied = copied.checked_add(read as u64).ok_or_else(limit)?;
        if copied > size {
            return Err(invalid_source());
        }
        content.update(&buffer[..read]);
        baseline_file.write_all(&buffer[..read])?;
        worktree_file.write_all(&buffer[..read])?;
    }
    buffer.fill(0);
    if copied != size
        || source.refresh_metadata().map_err(|_| invalid_source())? != expected
        || source.verify_path_identity().is_err()
    {
        return Err(invalid_source());
    }
    baseline_file.sync_all()?;
    worktree_file.sync_all()?;
    let digest: [u8; 32] = content.finalize().into();
    update_file_manifest(&mut state.manifest, relative, size, &digest)?;
    state.file_count += 1;
    state.logical_bytes += size;
    Ok(())
}

#[cfg(windows)]
fn same_windows_directory_entries(left: &[DirectoryEntry], right: &[DirectoryEntry]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            let left_structural = left.attributes & WINDOWS_STRUCTURAL_ATTRIBUTES;
            let right_structural = right.attributes & WINDOWS_STRUCTURAL_ATTRIBUTES;
            left.name == right.name
                && left.file_id == right.file_id
                && left.reparse_tag == right.reparse_tag
                && left_structural == right_structural
                && (left_structural & WINDOWS_DIRECTORY_ATTRIBUTE != 0
                    || (left.size == right.size
                        && left.last_write_time == right.last_write_time
                        && left.change_time == right.change_time))
        })
}

fn validate_repository_directory(
    repository: &Path,
    plan: &RepositoryImportPlan,
) -> Result<RepositoryImportOutcome, PersistenceError> {
    validate_private_directory(repository)?;
    let expected = read_import_metadata(repository, plan)?;
    let baseline = repository.join(BASELINE_DIRECTORY_NAME);
    let generation = repository
        .join(GENERATIONS_DIRECTORY_NAME)
        .join(encode_hex(&plan.generation_id));
    let worktree = generation.join(GENERATION_CONTENT_DIRECTORY_NAME);
    validate_private_directory(&baseline)?;
    validate_private_directory(&generation)?;
    validate_private_directory(&worktree)?;
    if read_generation_metadata(&generation, &plan.workspace_id, &plan.generation_id)? != expected {
        return Err(invalid_repository());
    }
    let actual = scan_repository_pair(&baseline, &worktree)?;
    if actual != expected {
        return Err(PersistenceError::InvalidState {
            reason: "repository import metadata does not match its trees",
        });
    }
    Ok(actual)
}

fn scan_repository_pair(
    baseline: &Path,
    worktree: &Path,
) -> Result<RepositoryImportOutcome, PersistenceError> {
    let mut state = ImportState {
        manifest: Sha256::new(),
        file_count: 0,
        directory_count: 0,
        logical_bytes: 0,
    };
    state.manifest.update(MANIFEST_CONTEXT);
    scan_pair_directory(baseline, worktree, &mut Vec::new(), &mut state)?;
    Ok(RepositoryImportOutcome {
        file_count: state.file_count,
        directory_count: state.directory_count,
        logical_bytes: state.logical_bytes,
        manifest_digest: state.manifest.finalize().into(),
    })
}

fn scan_pair_directory(
    baseline: &Path,
    worktree: &Path,
    components: &mut Vec<String>,
    state: &mut ImportState,
) -> Result<(), PersistenceError> {
    let mut entries = fs::read_dir(baseline)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by(|left, right| {
        left.file_name()
            .as_encoded_bytes()
            .cmp(right.file_name().as_encoded_bytes())
    });
    for entry in entries {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| PersistenceError::InvalidState {
                reason: "an imported repository path is not UTF-8",
            })?;
        components.push(name.clone());
        reserve_entry(state)?;
        let relative = relative_path_bytes(components)?;
        let baseline_path = baseline.join(&name);
        let worktree_path = worktree.join(&name);
        let baseline_metadata = fs::symlink_metadata(&baseline_path)?;
        let worktree_metadata = fs::symlink_metadata(&worktree_path)?;
        if ordinary_directory(&baseline_metadata) && ordinary_directory(&worktree_metadata) {
            state.directory_count += 1;
            update_directory_manifest(&mut state.manifest, &relative)?;
            scan_pair_directory(&baseline_path, &worktree_path, components, state)?;
        } else if ordinary_file(&baseline_metadata) && ordinary_file(&worktree_metadata) {
            if baseline_metadata.len() != worktree_metadata.len() {
                return Err(invalid_repository());
            }
            let (baseline_digest, size) = hash_file(&baseline_path)?;
            let (worktree_digest, worktree_size) = hash_file(&worktree_path)?;
            if size != worktree_size || baseline_digest != worktree_digest {
                return Err(invalid_repository());
            }
            update_file_manifest(&mut state.manifest, &relative, size, &baseline_digest)?;
            state.file_count += 1;
            state.logical_bytes = state.logical_bytes.checked_add(size).ok_or_else(limit)?;
        } else {
            return Err(invalid_repository());
        }
        components.pop();
    }
    let baseline_names = fs::read_dir(baseline)?.count();
    let worktree_names = fs::read_dir(worktree)?.count();
    if baseline_names != worktree_names {
        return Err(invalid_repository());
    }
    Ok(())
}

fn scan_baseline_tree(baseline: &Path) -> Result<RepositoryImportOutcome, PersistenceError> {
    let mut state = ImportState {
        manifest: Sha256::new(),
        file_count: 0,
        directory_count: 0,
        logical_bytes: 0,
    };
    state.manifest.update(MANIFEST_CONTEXT);
    scan_baseline_directory(baseline, &mut Vec::new(), &mut state)?;
    Ok(RepositoryImportOutcome {
        file_count: state.file_count,
        directory_count: state.directory_count,
        logical_bytes: state.logical_bytes,
        manifest_digest: state.manifest.finalize().into(),
    })
}

fn scan_baseline_directory(
    baseline: &Path,
    components: &mut Vec<String>,
    state: &mut ImportState,
) -> Result<(), PersistenceError> {
    let mut entries = fs::read_dir(baseline)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by(|left, right| {
        left.file_name()
            .as_encoded_bytes()
            .cmp(right.file_name().as_encoded_bytes())
    });
    for entry in entries {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid_repository())?;
        components.push(name.clone());
        reserve_entry(state)?;
        let relative = relative_path_bytes(components)?;
        let path = baseline.join(&name);
        let metadata = fs::symlink_metadata(&path)?;
        if ordinary_directory(&metadata) {
            state.directory_count += 1;
            update_directory_manifest(&mut state.manifest, &relative)?;
            scan_baseline_directory(&path, components, state)?;
        } else if ordinary_file(&metadata) {
            let (digest, size) = hash_file(&path)?;
            update_file_manifest(&mut state.manifest, &relative, size, &digest)?;
            state.file_count += 1;
            state.logical_bytes = state.logical_bytes.checked_add(size).ok_or_else(limit)?;
            if state.logical_bytes > MAX_LOGICAL_BYTES {
                return Err(limit());
            }
        } else {
            return Err(invalid_repository());
        }
        components.pop();
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<([u8; 32], u64), PersistenceError> {
    let metadata = fs::symlink_metadata(path)?;
    if !ordinary_file(&metadata) || metadata.len() > MAX_FILE_BYTES {
        return Err(invalid_repository());
    }
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size = size.checked_add(read as u64).ok_or_else(limit)?;
        if size > MAX_FILE_BYTES {
            return Err(limit());
        }
        digest.update(&buffer[..read]);
    }
    buffer.fill(0);
    Ok((digest.finalize().into(), size))
}

fn write_generation_metadata(
    generation: &Path,
    workspace_id: &[u8; 16],
    generation_id: &[u8; 16],
    outcome: RepositoryImportOutcome,
) -> Result<(), PersistenceError> {
    let path = generation.join(GENERATION_METADATA_FILE_NAME);
    let mut file = create_private_file(&path)?;
    file.write_all(GENERATION_METADATA_CONTEXT)?;
    file.write_all(workspace_id)?;
    file.write_all(generation_id)?;
    file.write_all(&outcome.file_count.to_be_bytes())?;
    file.write_all(&outcome.directory_count.to_be_bytes())?;
    file.write_all(&outcome.logical_bytes.to_be_bytes())?;
    file.write_all(&outcome.manifest_digest)?;
    file.sync_all()?;
    validate_private_file(&path, Some(GENERATION_METADATA_BYTES as u64))?;
    Ok(())
}

fn read_generation_metadata(
    generation: &Path,
    workspace_id: &[u8; 16],
    generation_id: &[u8; 16],
) -> Result<RepositoryImportOutcome, PersistenceError> {
    let path = generation.join(GENERATION_METADATA_FILE_NAME);
    validate_private_file(&path, Some(GENERATION_METADATA_BYTES as u64))?;
    let mut bytes = vec![0_u8; GENERATION_METADATA_BYTES];
    File::open(path)?.read_exact(&mut bytes)?;
    let mut offset = GENERATION_METADATA_CONTEXT.len();
    if &bytes[..offset] != GENERATION_METADATA_CONTEXT
        || bytes[offset..offset + 16] != *workspace_id
        || bytes[offset + 16..offset + 32] != *generation_id
    {
        return Err(invalid_repository());
    }
    offset += 32;
    let file_count = take_u64(&bytes, &mut offset)?;
    let directory_count = take_u64(&bytes, &mut offset)?;
    let logical_bytes = take_u64(&bytes, &mut offset)?;
    let manifest_digest = bytes[offset..offset + 32]
        .try_into()
        .map_err(|_| invalid_repository())?;
    Ok(RepositoryImportOutcome {
        file_count,
        directory_count,
        logical_bytes,
        manifest_digest,
    })
}

fn write_import_metadata(
    repository: &Path,
    plan: &RepositoryImportPlan,
    outcome: RepositoryImportOutcome,
) -> Result<(), PersistenceError> {
    write_import_metadata_at(&repository.join(IMPORT_METADATA_FILE_NAME), plan, outcome)
}

fn write_import_metadata_at(
    path: &Path,
    plan: &RepositoryImportPlan,
    outcome: RepositoryImportOutcome,
) -> Result<(), PersistenceError> {
    let mut file = create_private_file(path)?;
    file.write_all(METADATA_CONTEXT)?;
    file.write_all(&plan.workspace_id)?;
    file.write_all(&plan.operation_id)?;
    file.write_all(&plan.generation_id)?;
    file.write_all(&outcome.file_count.to_be_bytes())?;
    file.write_all(&outcome.directory_count.to_be_bytes())?;
    file.write_all(&outcome.logical_bytes.to_be_bytes())?;
    file.write_all(&outcome.manifest_digest)?;
    file.sync_all()?;
    validate_private_file(path, Some(METADATA_BYTES as u64))?;
    Ok(())
}

fn upgrade_import_metadata(
    repository: &Path,
    layout: &WorktreeLayoutPlan,
    outcome: RepositoryImportOutcome,
) -> Result<(), PersistenceError> {
    let import_plan = RepositoryImportPlan {
        request_id: crate::persistence::MutationRequestId::from_bytes([0; 16]),
        session_id: layout.session_id,
        workspace_id: layout.workspace_id,
        operation_id: read_legacy_import_operation_id(repository)?,
        generation_id: layout.generation_id,
        state: 2,
    };
    let current = read_import_metadata(repository, &import_plan)?;
    if current != outcome {
        return Err(invalid_repository());
    }
    let path = repository.join(IMPORT_METADATA_FILE_NAME);
    if fs::symlink_metadata(&path)?.len() == METADATA_BYTES as u64 {
        return Ok(());
    }
    let temporary = repository.join(IMPORT_METADATA_UPGRADE_FILE_NAME);
    if path_entry_exists(&temporary)? {
        validate_private_file(&temporary, None)?;
        fs::remove_file(&temporary)?;
    }
    write_import_metadata_at(&temporary, &import_plan, outcome)?;
    fs::rename(&temporary, &path)?;
    sync_directory(repository)?;
    Ok(())
}

fn read_legacy_import_operation_id(repository: &Path) -> Result<[u8; 16], PersistenceError> {
    let path = repository.join(IMPORT_METADATA_FILE_NAME);
    validate_private_file(&path, None)?;
    let mut prefix = vec![0_u8; LEGACY_METADATA_CONTEXT.len() + 32];
    File::open(path)?.read_exact(&mut prefix)?;
    let context_length = if prefix.starts_with(LEGACY_METADATA_CONTEXT) {
        LEGACY_METADATA_CONTEXT.len()
    } else if prefix.starts_with(METADATA_CONTEXT) {
        METADATA_CONTEXT.len()
    } else {
        return Err(invalid_repository());
    };
    prefix[context_length + 16..context_length + 32]
        .try_into()
        .map_err(|_| invalid_repository())
}

fn read_import_metadata(
    repository: &Path,
    plan: &RepositoryImportPlan,
) -> Result<RepositoryImportOutcome, PersistenceError> {
    let path = repository.join(IMPORT_METADATA_FILE_NAME);
    validate_private_file(&path, None)?;
    let length =
        usize::try_from(fs::symlink_metadata(&path)?.len()).map_err(|_| invalid_repository())?;
    if length != METADATA_BYTES && length != LEGACY_METADATA_BYTES {
        return Err(invalid_repository());
    }
    let mut bytes = vec![0_u8; length];
    File::open(path)?.read_exact(&mut bytes)?;
    let legacy = length == LEGACY_METADATA_BYTES;
    let context = if legacy {
        LEGACY_METADATA_CONTEXT
    } else {
        METADATA_CONTEXT
    };
    let mut offset = context.len();
    if &bytes[..offset] != context
        || bytes[offset..offset + 16] != plan.workspace_id
        || bytes[offset + 16..offset + 32] != plan.operation_id
        || (!legacy && bytes[offset + 32..offset + 48] != plan.generation_id)
    {
        return Err(invalid_repository());
    }
    offset += if legacy { 32 } else { 48 };
    let file_count = take_u64(&bytes, &mut offset)?;
    let directory_count = take_u64(&bytes, &mut offset)?;
    let logical_bytes = take_u64(&bytes, &mut offset)?;
    let manifest_digest = bytes[offset..offset + 32]
        .try_into()
        .map_err(|_| invalid_repository())?;
    Ok(RepositoryImportOutcome {
        file_count,
        directory_count,
        logical_bytes,
        manifest_digest,
    })
}

fn take_u64(bytes: &[u8], offset: &mut usize) -> Result<u64, PersistenceError> {
    let end = offset.checked_add(8).ok_or_else(invalid_repository)?;
    let value = u64::from_be_bytes(
        bytes
            .get(*offset..end)
            .ok_or_else(invalid_repository)?
            .try_into()
            .map_err(|_| invalid_repository())?,
    );
    *offset = end;
    Ok(value)
}

fn update_directory_manifest(digest: &mut Sha256, relative: &[u8]) -> Result<(), PersistenceError> {
    digest.update([0]);
    digest.update(path_length(relative)?.to_be_bytes());
    digest.update(relative);
    Ok(())
}

fn update_file_manifest(
    digest: &mut Sha256,
    relative: &[u8],
    size: u64,
    content_digest: &[u8; 32],
) -> Result<(), PersistenceError> {
    digest.update([1]);
    digest.update(path_length(relative)?.to_be_bytes());
    digest.update(relative);
    digest.update(size.to_be_bytes());
    digest.update(content_digest);
    Ok(())
}

fn path_length(relative: &[u8]) -> Result<u32, PersistenceError> {
    u32::try_from(relative.len()).map_err(|_| limit())
}

fn validate_component(name: &str, components: &[String]) -> Result<(), PersistenceError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || components.len() >= MAX_PATH_DEPTH
        || name.chars().any(char::is_control)
    {
        return Err(invalid_source());
    }
    Ok(())
}

fn relative_path_bytes(components: &[String]) -> Result<Vec<u8>, PersistenceError> {
    let relative = components.join("/").into_bytes();
    if relative.len() > MAX_RELATIVE_PATH_BYTES {
        return Err(limit());
    }
    Ok(relative)
}

fn reserve_entry(state: &ImportState) -> Result<(), PersistenceError> {
    if state.file_count + state.directory_count >= MAX_ENTRIES {
        return Err(limit());
    }
    Ok(())
}

fn staging_name(operation_id: &[u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut name = String::with_capacity(STAGING_PREFIX.len() + 32);
    name.push_str(STAGING_PREFIX);
    for byte in operation_id {
        name.push(char::from(HEX[usize::from(byte >> 4)]));
        name.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    name
}

fn remove_confined_tree(workspace: &Path, path: &Path) -> Result<(), PersistenceError> {
    if path.parent() != Some(workspace)
        || !path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| {
                name.starts_with(STAGING_PREFIX) && name.len() == STAGING_PREFIX.len() + 32
            })
    {
        return Err(invalid_repository());
    }
    validate_private_directory(path)?;
    fs::remove_dir_all(path)?;
    sync_directory(workspace)?;
    Ok(())
}

fn ordinary_file(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_file() && !is_reparse(metadata)
}

fn ordinary_directory(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_dir() && !metadata.file_type().is_symlink() && !is_reparse(metadata)
}

#[cfg(unix)]
fn is_reparse(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(windows)]
fn is_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_attributes() & WINDOWS_REPARSE_ATTRIBUTE != 0
}

#[cfg(unix)]
fn same_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino() && left.file_type() == right.file_type()
}

#[cfg(unix)]
fn modified_value(metadata: &fs::Metadata) -> Option<std::time::SystemTime> {
    metadata.modified().ok()
}

fn invalid_source() -> PersistenceError {
    PersistenceError::InvalidInput {
        reason: "the repository source tree is invalid or changed during import",
    }
}

fn invalid_repository() -> PersistenceError {
    PersistenceError::InvalidState {
        reason: "repository import state is invalid",
    }
}

fn limit() -> PersistenceError {
    PersistenceError::ResourceLimit {
        resource: PersistenceResourceLimit::Workspace,
    }
}

struct ImportState {
    manifest: Sha256,
    file_count: u64,
    directory_count: u64,
    logical_bytes: u64,
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::ffi::OsString;

    use super::*;

    fn entry() -> DirectoryEntry {
        DirectoryEntry {
            name: OsString::from("source.rs"),
            file_id: [9; 16],
            attributes: 0x20,
            reparse_tag: None,
            size: 17,
            allocation_size: 24,
            creation_time: 18,
            last_write_time: 19,
            change_time: 20,
        }
    }

    #[test]
    fn windows_directory_snapshot_ignores_incidental_metadata_changes() {
        let expected_entry = entry();
        let mut observed_entry = expected_entry.clone();
        observed_entry.allocation_size = 32;
        observed_entry.attributes = 0x21;
        observed_entry.creation_time = 21;
        assert!(same_windows_directory_entries(
            &[expected_entry],
            &[observed_entry]
        ));
    }

    #[test]
    fn windows_directory_snapshot_rejects_security_relevant_changes() {
        let expected_entry = entry();
        let mut changed_entry = expected_entry.clone();
        changed_entry.attributes |= WINDOWS_REPARSE_ATTRIBUTE;
        assert!(!same_windows_directory_entries(
            std::slice::from_ref(&expected_entry),
            &[changed_entry]
        ));
        let mut changed_entry = expected_entry.clone();
        changed_entry.change_time += 1;
        assert!(!same_windows_directory_entries(
            &[expected_entry],
            &[changed_entry]
        ));
    }
}
