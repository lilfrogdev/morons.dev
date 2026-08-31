use std::{fmt, fs, fs::File, io::Read as _, path::Path};

use zeroize::{Zeroize, Zeroizing};

use super::super::{
    OpenCodeCredentialStatus, PersistenceError, paths::validate_private_file,
    types::IDENTIFIER_BYTES,
};

const CREDENTIAL_CONTEXT: &[u8] = b"morons.dev/opencode-credential/v1\0";
const CREDENTIAL_STATE_REMOVED: u8 = 0;
const CREDENTIAL_STATE_CONFIGURED: u8 = 1;
const MAX_API_KEY_BYTES: usize = 4096;
const CREDENTIAL_HEADER_BYTES: usize = CREDENTIAL_CONTEXT.len() + 1 + 8 + IDENTIFIER_BYTES + 4;
pub(super) const MAX_CREDENTIAL_FILE_BYTES: usize = CREDENTIAL_HEADER_BYTES + MAX_API_KEY_BYTES;
const MAX_CREDENTIAL_GENERATION: u64 = i64::MAX as u64;

pub(in crate::persistence) struct StoredOpenCodeApiKey(Vec<u8>);

impl StoredOpenCodeApiKey {
    pub(in crate::persistence) fn new(bytes: Vec<u8>) -> Result<Self, PersistenceError> {
        if !valid_api_key(&bytes) {
            let mut bytes = bytes;
            bytes.zeroize();
            return Err(PersistenceError::InvalidInput {
                reason: "an OpenCode API key must contain between 1 and 4096 visible ASCII bytes",
            });
        }
        Ok(Self(bytes))
    }

    fn from_persisted(mut bytes: Vec<u8>) -> Result<Self, PersistenceError> {
        if !valid_api_key(&bytes) {
            bytes.zeroize();
            return Err(PersistenceError::InvalidState {
                reason: "the persisted OpenCode API key is invalid",
            });
        }
        Ok(Self(bytes))
    }

    pub(in crate::persistence) fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub(in crate::persistence) fn clone_for_dispatch(&self) -> Self {
        Self(self.0.clone())
    }
}

impl fmt::Debug for StoredOpenCodeApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StoredOpenCodeApiKey([REDACTED])")
    }
}

impl Drop for StoredOpenCodeApiKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Debug)]
pub(in crate::persistence) struct CredentialState {
    generation: u64,
    mutation_marker: [u8; IDENTIFIER_BYTES],
    api_key: Option<StoredOpenCodeApiKey>,
}

impl CredentialState {
    pub(super) fn new(
        generation: u64,
        mutation_marker: [u8; IDENTIFIER_BYTES],
        api_key: Option<StoredOpenCodeApiKey>,
    ) -> Self {
        Self {
            generation,
            mutation_marker,
            api_key,
        }
    }

    pub(super) fn unconfigured() -> Self {
        Self {
            generation: 0,
            mutation_marker: [0; IDENTIFIER_BYTES],
            api_key: None,
        }
    }

    pub(super) const fn generation(&self) -> u64 {
        self.generation
    }

    pub(in crate::persistence) const fn mutation_marker(&self) -> &[u8; IDENTIFIER_BYTES] {
        &self.mutation_marker
    }

    pub(super) fn status(&self) -> OpenCodeCredentialStatus {
        OpenCodeCredentialStatus {
            configured: self.api_key.is_some(),
            generation: self.generation,
        }
    }

    pub(super) fn clone_api_key_for_dispatch(&self) -> Option<StoredOpenCodeApiKey> {
        self.api_key
            .as_ref()
            .map(StoredOpenCodeApiKey::clone_for_dispatch)
    }
}

pub(super) fn encode_state(state: &CredentialState) -> Result<Vec<u8>, PersistenceError> {
    if state.generation == 0 || state.generation > MAX_CREDENTIAL_GENERATION {
        return Err(PersistenceError::InvalidState {
            reason: "the credential generation is invalid",
        });
    }
    if state.mutation_marker.iter().all(|byte| *byte == 0) {
        return Err(PersistenceError::InvalidState {
            reason: "the credential mutation marker is invalid",
        });
    }
    let key_bytes = state
        .api_key
        .as_ref()
        .map_or(&[][..], StoredOpenCodeApiKey::as_bytes);
    let key_length =
        u32::try_from(key_bytes.len()).map_err(|_| PersistenceError::InvalidState {
            reason: "the OpenCode API key length is invalid",
        })?;
    let mut payload = Vec::with_capacity(CREDENTIAL_HEADER_BYTES + key_bytes.len());
    payload.extend_from_slice(CREDENTIAL_CONTEXT);
    payload.push(if state.api_key.is_some() {
        CREDENTIAL_STATE_CONFIGURED
    } else {
        CREDENTIAL_STATE_REMOVED
    });
    payload.extend_from_slice(&state.generation.to_be_bytes());
    payload.extend_from_slice(&state.mutation_marker);
    payload.extend_from_slice(&key_length.to_be_bytes());
    payload.extend_from_slice(key_bytes);
    Ok(payload)
}

pub(super) fn read_state(path: &Path) -> Result<CredentialState, PersistenceError> {
    validate_private_file(path, Some(MAX_CREDENTIAL_FILE_BYTES as u64))?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.len() < CREDENTIAL_HEADER_BYTES as u64 {
        return Err(PersistenceError::InvalidState {
            reason: "the credential state is truncated",
        });
    }
    let mut payload = Zeroizing::new(Vec::with_capacity(metadata.len() as usize));
    File::open(path)?
        .take(MAX_CREDENTIAL_FILE_BYTES as u64 + 1)
        .read_to_end(&mut payload)?;
    if payload.len() > MAX_CREDENTIAL_FILE_BYTES {
        return Err(PersistenceError::InvalidState {
            reason: "the credential state exceeds its size limit",
        });
    }
    decode_state(&payload)
}

fn decode_state(payload: &[u8]) -> Result<CredentialState, PersistenceError> {
    let Some(rest) = payload.strip_prefix(CREDENTIAL_CONTEXT) else {
        return Err(PersistenceError::InvalidState {
            reason: "the credential state has an unsupported version",
        });
    };
    if rest.len() < 1 + 8 + IDENTIFIER_BYTES + 4 {
        return Err(PersistenceError::InvalidState {
            reason: "the credential state is truncated",
        });
    }
    let state_tag = rest[0];
    let generation =
        u64::from_be_bytes(
            rest[1..9]
                .try_into()
                .map_err(|_| PersistenceError::InvalidState {
                    reason: "the credential generation is malformed",
                })?,
        );
    if generation == 0 || generation > MAX_CREDENTIAL_GENERATION {
        return Err(PersistenceError::InvalidState {
            reason: "the credential generation is invalid",
        });
    }
    let mutation_marker: [u8; IDENTIFIER_BYTES] = rest[9..9 + IDENTIFIER_BYTES]
        .try_into()
        .map_err(|_| PersistenceError::InvalidState {
            reason: "the credential mutation marker is malformed",
        })?;
    if mutation_marker.iter().all(|byte| *byte == 0) {
        return Err(PersistenceError::InvalidState {
            reason: "the credential mutation marker is invalid",
        });
    }
    let key_length_offset = 9 + IDENTIFIER_BYTES;
    let key_length = usize::try_from(u32::from_be_bytes(
        rest[key_length_offset..key_length_offset + 4]
            .try_into()
            .map_err(|_| PersistenceError::InvalidState {
                reason: "the OpenCode API key length is malformed",
            })?,
    ))
    .map_err(|_| PersistenceError::InvalidState {
        reason: "the OpenCode API key length is invalid",
    })?;
    let key_bytes = &rest[key_length_offset + 4..];
    if key_bytes.len() != key_length {
        return Err(PersistenceError::InvalidState {
            reason: "the credential state has an inconsistent length",
        });
    }
    let api_key = match state_tag {
        CREDENTIAL_STATE_REMOVED if key_bytes.is_empty() => None,
        CREDENTIAL_STATE_CONFIGURED if !key_bytes.is_empty() => {
            Some(StoredOpenCodeApiKey::from_persisted(key_bytes.to_vec())?)
        }
        CREDENTIAL_STATE_REMOVED | CREDENTIAL_STATE_CONFIGURED => {
            return Err(PersistenceError::InvalidState {
                reason: "the credential state payload is inconsistent",
            });
        }
        _ => {
            return Err(PersistenceError::InvalidState {
                reason: "the credential state tag is unsupported",
            });
        }
    };
    Ok(CredentialState::new(generation, mutation_marker, api_key))
}

pub(super) fn validate_installed_state(
    actual: &CredentialState,
    expected: &CredentialState,
) -> Result<(), PersistenceError> {
    let keys_match = match (&actual.api_key, &expected.api_key) {
        (Some(actual), Some(expected)) => constant_time_eq(actual.as_bytes(), expected.as_bytes()),
        (None, None) => true,
        _ => false,
    };
    if actual.generation != expected.generation
        || actual.mutation_marker != expected.mutation_marker
        || !keys_match
    {
        return Err(PersistenceError::InvalidState {
            reason: "the installed credential state does not match the update",
        });
    }
    Ok(())
}

fn valid_api_key(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes.len() <= MAX_API_KEY_BYTES
        && bytes.iter().all(|byte| matches!(byte, 0x21..=0x7e))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}
