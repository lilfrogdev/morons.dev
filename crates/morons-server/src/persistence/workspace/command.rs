use std::{ffi::OsStr, fs, path::PathBuf};

use crate::persistence::PersistenceError;

use crate::persistence::paths::{
    StoragePaths, encode_hex, path_entry_exists, sync_directory, validate_private_directory,
};

const OPERATION_PREFIX: &str = "command-";

impl StoragePaths {
    /// Remove operation state left by the superseded sandbox-command implementation.
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
