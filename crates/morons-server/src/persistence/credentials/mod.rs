mod format;

use std::{
    ffi::OsStr,
    fs::{self},
    path::{Path, PathBuf},
};

use zeroize::Zeroizing;

use self::format::{
    CredentialState, MAX_CREDENTIAL_FILE_BYTES, encode_state, read_state, validate_installed_state,
};
use super::{
    OpenCodeCredentialStatus, PersistenceError, PersistenceResourceLimit,
    paths::{
        create_private_file, encode_hex, ensure_private_directory, path_entry_exists,
        sync_directory, validate_private_file,
    },
    types::IDENTIFIER_BYTES,
};

pub(in crate::persistence) use self::format::StoredOpenCodeApiKey;

const CREDENTIAL_DIRECTORY_NAME: &str = "credentials";
const CREDENTIAL_FILE_NAME: &str = "opencode.state";
const CREDENTIAL_TEMPORARY_PREFIX: &str = ".opencode.state-";
const CREDENTIAL_TEMPORARY_SUFFIX: &str = ".tmp";
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
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_name = entry.file_name();
        if file_name == OsStr::new(CREDENTIAL_FILE_NAME) {
            validate_private_file(&entry.path(), Some(MAX_CREDENTIAL_FILE_BYTES as u64))?;
            continue;
        }
        if is_temporary_file_name(&file_name) {
            validate_private_file(&entry.path(), Some(MAX_CREDENTIAL_FILE_BYTES as u64))?;
            fs::remove_file(entry.path())?;
            removed_temporary_file = true;
            continue;
        }
        return Err(PersistenceError::InvalidState {
            reason: "the credential directory contains unexpected state",
        });
    }
    if removed_temporary_file {
        sync_directory(directory)?;
    }
    Ok(())
}

fn write_state(directory: &Path, state: &CredentialState) -> Result<(), PersistenceError> {
    let temporary_path = directory.join(format!(
        "{CREDENTIAL_TEMPORARY_PREFIX}{}{CREDENTIAL_TEMPORARY_SUFFIX}",
        encode_hex(state.mutation_marker())
    ));
    let credential_path = directory.join(CREDENTIAL_FILE_NAME);
    let payload = Zeroizing::new(encode_state(state)?);
    let mut file = create_private_file(&temporary_path)?;
    let result = (|| -> Result<(), PersistenceError> {
        use std::io::Write as _;

        file.write_all(&payload)?;
        file.sync_all()?;
        drop(file);
        validate_private_file(&temporary_path, Some(payload.len() as u64))?;
        fs::rename(&temporary_path, &credential_path)?;
        sync_directory(directory)?;
        let installed = read_state(&credential_path)?;
        validate_installed_state(&installed, state)
    })();
    if result.is_err() && path_entry_exists(&temporary_path).unwrap_or(false) {
        let _ = fs::remove_file(&temporary_path);
        let _ = sync_directory(directory);
    }
    result
}

fn is_temporary_file_name(file_name: &OsStr) -> bool {
    let Some(file_name) = file_name.to_str() else {
        return false;
    };
    let Some(encoded_marker) = file_name
        .strip_prefix(CREDENTIAL_TEMPORARY_PREFIX)
        .and_then(|name| name.strip_suffix(CREDENTIAL_TEMPORARY_SUFFIX))
    else {
        return false;
    };
    encoded_marker.len() == IDENTIFIER_BYTES * 2
        && encoded_marker
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
