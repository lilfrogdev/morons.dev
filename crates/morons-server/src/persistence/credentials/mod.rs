mod files;
mod format;
pub(in crate::persistence) mod openai;

use std::{
    fs::{self},
    path::{Path, PathBuf},
};

use self::format::{CredentialState, encode_state, read_state, validate_installed_state};
use super::{
    CredentialKind, OpenCodeCredentialStatus, PersistenceError, PersistenceResourceLimit,
    paths::{ensure_private_directory, path_entry_exists, sync_directory, validate_private_file},
    types::IDENTIFIER_BYTES,
};

pub(in crate::persistence) use self::format::StoredOpenCodeApiKey;

const CREDENTIAL_DIRECTORY_NAME: &str = "credentials";
const CREDENTIAL_FILE_NAME: &str = "opencode.state";
const MAX_CREDENTIAL_GENERATION: u64 = i64::MAX as u64;

pub(super) struct CredentialStore {
    directory: PathBuf,
    state: CredentialState,
    consistent: bool,
}

impl CredentialStore {
    pub(super) fn open(application_root: &Path) -> Result<Self, PersistenceError> {
        let directory = application_root.join(CREDENTIAL_DIRECTORY_NAME);
        ensure_private_directory(&directory)?;
        cleanup_and_validate_directory(&directory)?;
        let credential_path = directory.join(CREDENTIAL_FILE_NAME);
        let state = if path_entry_exists(&credential_path)? {
            read_state(&credential_path)?
        } else {
            CredentialState::unconfigured()
        };
        Ok(Self {
            directory,
            state,
            consistent: true,
        })
    }

    pub(super) fn ensure_consistent(&self) -> Result<(), PersistenceError> {
        if !self.consistent {
            return Err(PersistenceError::InvalidState {
                reason: "credential state could not be reconciled after a filesystem error",
            });
        }
        Ok(())
    }

    pub(super) const fn is_consistent(&self) -> bool {
        self.consistent
    }

    pub(super) fn status(&self) -> OpenCodeCredentialStatus {
        self.state.status()
    }

    pub(super) fn state(&self) -> &CredentialState {
        &self.state
    }

    pub(super) fn clone_key_for_dispatch(
        &self,
        expected_generation: u64,
    ) -> Result<StoredOpenCodeApiKey, PersistenceError> {
        self.ensure_consistent()?;
        if expected_generation != self.state.generation() {
            return Err(PersistenceError::CredentialGenerationConflict);
        }
        self.state
            .clone_api_key_for_dispatch()
            .ok_or(PersistenceError::CredentialNotConfigured)
    }

    pub(super) fn apply(
        &mut self,
        expected_generation: u64,
        mutation_marker: [u8; IDENTIFIER_BYTES],
        api_key: Option<StoredOpenCodeApiKey>,
    ) -> Result<OpenCodeCredentialStatus, PersistenceError> {
        self.ensure_consistent()?;
        if expected_generation != self.state.generation() {
            return Err(PersistenceError::CredentialGenerationConflict);
        }
        if mutation_marker.iter().all(|byte| *byte == 0) {
            return Err(PersistenceError::InvalidInput {
                reason: "a credential mutation marker must not be all zeroes",
            });
        }
        let generation = expected_generation
            .checked_add(1)
            .filter(|generation| *generation <= MAX_CREDENTIAL_GENERATION)
            .ok_or(PersistenceError::ResourceLimit {
                resource: PersistenceResourceLimit::CredentialGeneration,
            })?;
        let next = CredentialState::new(generation, mutation_marker, api_key);
        if let Err(error) = write_state(&self.directory, &next) {
            if let Err(reload_error) = self.reload() {
                self.consistent = false;
                return Err(reload_error);
            }
            return Err(error);
        }
        self.state = next;
        Ok(self.state.status())
    }

    fn reload(&mut self) -> Result<(), PersistenceError> {
        let credential_path = self.directory.join(CREDENTIAL_FILE_NAME);
        self.state = if path_entry_exists(&credential_path)? {
            read_state(&credential_path)?
        } else {
            CredentialState::unconfigured()
        };
        self.consistent = true;
        Ok(())
    }
}

fn cleanup_and_validate_directory(directory: &Path) -> Result<(), PersistenceError> {
    let mut removed_temporary_file = false;
    for (index, entry) in fs::read_dir(directory)?.enumerate() {
        if index >= 128 {
            return Err(PersistenceError::InvalidState {
                reason: "the credential directory exceeds its entry bound",
            });
        }
        let entry = entry?;
        let file_name = entry.file_name();
        let name = file_name.to_str().ok_or(PersistenceError::InvalidState {
            reason: "the credential directory contains unexpected state",
        })?;
        let kind = [CredentialKind::OpenCode, CredentialKind::OpenAiChatGpt]
            .into_iter()
            .find(|kind| name == files::name(*kind) || files::is_temporary(name, *kind))
            .ok_or(PersistenceError::InvalidState {
                reason: "the credential directory contains unexpected state",
            })?;
        validate_private_file(&entry.path(), Some(files::maximum(kind) as u64))?;
        if files::is_temporary(name, kind) {
            fs::remove_file(entry.path())?;
            removed_temporary_file = true;
        }
    }
    if removed_temporary_file {
        sync_directory(directory)?;
    }
    Ok(())
}

fn write_state(directory: &Path, state: &CredentialState) -> Result<(), PersistenceError> {
    let payload = zeroize::Zeroizing::new(encode_state(state)?);
    files::replace(
        directory,
        CredentialKind::OpenCode,
        state.mutation_marker(),
        &payload,
    )?;
    validate_installed_state(&read_state(&directory.join(CREDENTIAL_FILE_NAME))?, state)
}
