mod command;
mod repository_import;

pub(crate) use repository_import::{RepositoryRecovery, WorktreeLayoutRecovery};

use std::{
    ffi::OsStr,
    fs::{self, File},
    io::{Read, Write},
    path::Path,
};

use super::{
    paths::{
        PathError, StoragePaths, create_private_file, encode_hex, ensure_private_directory,
        path_entry_exists, sync_directory, validate_private_directory, validate_private_file,
    },
    types::IDENTIFIER_BYTES,
};

const WORKSPACE_IDENTITY_FILE_NAME: &str = "identity";
const WORKSPACE_IDENTITY_TEMPORARY_FILE_NAME: &str = ".identity.initializing";
const WORKSPACE_IDENTITY_CONTEXT: &[u8] = b"morons.dev/workspace/v1\0";

impl StoragePaths {
    pub(crate) fn provision_workspace(
        &self,
        workspace_id: &[u8; IDENTIFIER_BYTES],
    ) -> Result<(), PathError> {
        let workspace_path = self.workspace_directory.join(encode_hex(workspace_id));
        ensure_private_directory(&workspace_path)?;

        let identity_path = workspace_path.join(WORKSPACE_IDENTITY_FILE_NAME);
        let temporary_path = workspace_path.join(WORKSPACE_IDENTITY_TEMPORARY_FILE_NAME);
        if path_entry_exists(&identity_path)? {
            validate_workspace_identity(&identity_path, workspace_id)?;
            remove_workspace_identity_temporary_file(&temporary_path)?;
            return Ok(());
        }

        for entry in fs::read_dir(&workspace_path)? {
            let entry = entry?;
            if entry.file_name() != OsStr::new(WORKSPACE_IDENTITY_TEMPORARY_FILE_NAME) {
                return Err(PathError::InvalidState {
                    reason: "an uninitialized workspace contains unexpected state",
                });
            }
        }
        remove_workspace_identity_temporary_file(&temporary_path)?;

        let mut file = create_private_file(&temporary_path)?;
        file.write_all(WORKSPACE_IDENTITY_CONTEXT)?;
        file.write_all(workspace_id)?;
        file.sync_all()?;
        drop(file);

        if path_entry_exists(&identity_path)? {
            return Err(PathError::InvalidState {
                reason: "a workspace identity appeared during initialization",
            });
        }
        fs::rename(&temporary_path, &identity_path)?;
        sync_directory(&workspace_path)?;
        validate_workspace_identity(&identity_path, workspace_id)
    }

    pub(crate) fn validate_workspace(
        &self,
        workspace_id: &[u8; IDENTIFIER_BYTES],
        repository_expected: bool,
        repository_blocked: bool,
    ) -> Result<(), PathError> {
        let workspace_path = self.workspace_directory.join(encode_hex(workspace_id));
        validate_private_directory(&workspace_path)?;
        validate_workspace_identity(
            &workspace_path.join(WORKSPACE_IDENTITY_FILE_NAME),
            workspace_id,
        )?;
        for entry in fs::read_dir(&workspace_path)? {
            let name = entry?.file_name();
            if name == OsStr::new(WORKSPACE_IDENTITY_FILE_NAME)
                || repository_expected && name == OsStr::new("repository")
                || repository_blocked && is_repository_staging_name(&name)
            {
                continue;
            }
            return Err(PathError::InvalidState {
                reason: "a workspace contains unexpected state",
            });
        }
        Ok(())
    }
}

fn is_repository_staging_name(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some(identifier) = name.strip_prefix(".repository-importing-") else {
        return false;
    };
    identifier.len() == 32
        && identifier
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn validate_workspace_identity(
    path: &Path,
    expected_id: &[u8; IDENTIFIER_BYTES],
) -> Result<(), PathError> {
    validate_private_file(path, None)?;
    let expected_bytes = WORKSPACE_IDENTITY_CONTEXT.len() + IDENTIFIER_BYTES;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.len() != expected_bytes as u64 {
        return Err(PathError::InvalidState {
            reason: "a workspace identity has an unexpected length",
        });
    }

    let mut bytes = Vec::with_capacity(expected_bytes);
    File::open(path)?.read_to_end(&mut bytes)?;
    let (context, workspace_id) = bytes.split_at(WORKSPACE_IDENTITY_CONTEXT.len());
    if context != WORKSPACE_IDENTITY_CONTEXT || workspace_id != expected_id {
        return Err(PathError::InvalidState {
            reason: "a workspace identity does not match its durable record",
        });
    }
    Ok(())
}

fn remove_workspace_identity_temporary_file(path: &Path) -> Result<(), PathError> {
    if path_entry_exists(path)? {
        validate_private_file(path, None)?;
        fs::remove_file(path)?;
        if let Some(parent) = path.parent() {
            sync_directory(parent)?;
        }
    }
    Ok(())
}
