use std::{ffi::OsStr, fs, path::PathBuf};

use crate::persistence::{PersistenceError, RepositoryImportOutcome};

use super::repository_import::{copy_command_tree, write_command_generation_metadata};
use crate::persistence::paths::{
    StoragePaths, encode_hex, ensure_private_directory, path_entry_exists, sync_directory,
    validate_private_directory,
};

const OPERATION_PREFIX: &str = "command-";

pub(crate) struct CommandWorkspace {
    pub candidate: PathBuf,
    pub scratch: PathBuf,
    pub source_outcome: RepositoryImportOutcome,
}

impl StoragePaths {
    pub(crate) fn prepare_command_workspace(
        &self,
        workspace_id: &[u8; 16],
        active_generation: &[u8; 16],
        operation_id: &[u8; 16],
    ) -> Result<CommandWorkspace, PersistenceError> {
        let operation_root = self
            .sandbox_operation_directory
            .join(format!("{OPERATION_PREFIX}{}", encode_hex(operation_id)));
        if path_entry_exists(&operation_root)? {
            return Err(PersistenceError::InvalidState {
                reason: "command operation staging already exists",
            });
        }
        ensure_private_directory(&operation_root)?;
        let candidate = operation_root.join("candidate");
        let mirror = operation_root.join("candidate-mirror");
        let scratch = operation_root.join("scratch");
        ensure_private_directory(&candidate)?;
        ensure_private_directory(&mirror)?;
        ensure_private_directory(&scratch)?;
        let active = self.worktree_generation_path(workspace_id, active_generation);
        let source_outcome = match copy_command_tree(&active, &mirror, &candidate) {
            Ok(outcome) => outcome,
            Err(error) => {
                let _ = self.remove_command_workspace(&operation_root);
                return Err(error);
            }
        };
        sync_directory(&candidate)?;
        sync_directory(&operation_root)?;
        Ok(CommandWorkspace {
            candidate,
            scratch,
            source_outcome,
        })
    }

    pub(crate) fn publish_command_generation(
        &self,
        workspace_id: &[u8; 16],
        generation_id: &[u8; 16],
        operation_id: &[u8; 16],
        candidate: &std::path::Path,
    ) -> Result<RepositoryImportOutcome, PersistenceError> {
        let operation_root = self.command_operation_path(operation_id)?;
        if candidate.parent() != Some(operation_root.as_path())
            || candidate.file_name() != Some(OsStr::new("candidate"))
        {
            return Err(PersistenceError::InvalidState {
                reason: "command candidate escaped operation staging",
            });
        }
        let generation = operation_root.join("clean-generation");
        let content = generation.join("content");
        let mirror = operation_root.join("clean-mirror");
        ensure_private_directory(&generation)?;
        ensure_private_directory(&content)?;
        ensure_private_directory(&mirror)?;
        let outcome = copy_command_tree(candidate, &mirror, &content)?;
        write_command_generation_metadata(&generation, workspace_id, generation_id, outcome)?;
        sync_directory(&content)?;
        sync_directory(&generation)?;
        let repository_generations = self
            .workspace_path(workspace_id)
            .join("repository")
            .join("generations");
        validate_private_directory(&repository_generations)?;
        let destination = repository_generations.join(encode_hex(generation_id));
        if path_entry_exists(&destination)? {
            return Err(PersistenceError::InvalidState {
                reason: "a command generation destination already exists",
            });
        }
        fs::rename(&generation, &destination)?;
        sync_directory(&repository_generations)?;
        Ok(outcome)
    }

    pub(crate) fn validate_sandbox_operations_empty(&self) -> Result<(), PersistenceError> {
        validate_private_directory(&self.sandbox_operation_directory)?;
        if fs::read_dir(&self.sandbox_operation_directory)?
            .next()
            .is_some()
        {
            return Err(PersistenceError::InvalidState {
                reason: "the sandbox operation root contains unbound state",
            });
        }
        Ok(())
    }

    pub(crate) fn remove_command_operation(
        &self,
        operation_id: &[u8; 16],
    ) -> Result<(), PersistenceError> {
        let path = self.command_operation_path(operation_id)?;
        if path_entry_exists(&path)? {
            self.remove_command_workspace(&path)?;
        }
        Ok(())
    }

    pub(crate) fn remove_unreferenced_generation(
        &self,
        workspace_id: &[u8; 16],
        generation_id: &[u8; 16],
    ) -> Result<(), PersistenceError> {
        let path = self
            .workspace_path(workspace_id)
            .join("repository/generations")
            .join(encode_hex(generation_id));
        if path_entry_exists(&path)? {
            validate_private_directory(&path)?;
            fs::remove_dir_all(&path)?;
            sync_directory(path.parent().ok_or(PersistenceError::InvalidState {
                reason: "a generation has no parent",
            })?)?;
        }
        Ok(())
    }

    fn command_operation_path(&self, operation_id: &[u8; 16]) -> Result<PathBuf, PersistenceError> {
        let path = self
            .sandbox_operation_directory
            .join(format!("{OPERATION_PREFIX}{}", encode_hex(operation_id)));
        if path.parent() != Some(self.sandbox_operation_directory.as_path()) {
            return Err(PersistenceError::InvalidState {
                reason: "command operation path escaped its root",
            });
        }
        Ok(path)
    }

    fn remove_command_workspace(&self, path: &std::path::Path) -> Result<(), PersistenceError> {
        let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
        if path.parent() != Some(self.sandbox_operation_directory.as_path())
            || !name.starts_with(OPERATION_PREFIX)
            || name.len() != OPERATION_PREFIX.len() + 32
        {
            return Err(PersistenceError::InvalidState {
                reason: "command operation cleanup escaped its root",
            });
        }
        validate_private_directory(path)?;
        fs::remove_dir_all(path)?;
        sync_directory(&self.sandbox_operation_directory)?;
        Ok(())
    }
}
