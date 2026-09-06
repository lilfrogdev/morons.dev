use crate::persistence::{
    CredentialKind, PersistenceError,
    paths::{
        create_private_file, encode_hex, path_entry_exists, sync_directory, validate_private_file,
    },
};
use std::{
    fs,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

fn invalid() -> PersistenceError {
    PersistenceError::InvalidState {
        reason: "credential file replacement is invalid",
    }
}

pub(super) fn name(kind: CredentialKind) -> &'static str {
    match kind {
        CredentialKind::OpenCode => "opencode.state",
        CredentialKind::OpenAiChatGpt => "openai-chatgpt.state",
    }
}
pub(super) fn temporary(kind: CredentialKind, marker: &[u8; 16]) -> String {
    format!(".{}-{}.tmp", name(kind), encode_hex(marker))
}
pub(super) fn is_temporary(name: &str, kind: CredentialKind) -> bool {
    name.strip_prefix(&format!(".{}-", self::name(kind)))
        .and_then(|s| s.strip_suffix(".tmp"))
        .is_some_and(|s| {
            s.len() == 32
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}
pub(super) fn maximum(kind: CredentialKind) -> usize {
    match kind {
        CredentialKind::OpenCode => super::format::MAX_CREDENTIAL_FILE_BYTES,
        CredentialKind::OpenAiChatGpt => super::openai::format::MAX_FILE,
    }
}
pub(super) fn path(directory: &Path, kind: CredentialKind) -> PathBuf {
    directory.join(name(kind))
}

pub(super) fn read(path: &Path, maximum: usize) -> Result<Zeroizing<Vec<u8>>, PersistenceError> {
    validate_private_file(path, Some(maximum as u64))?;
    let mut bytes = Zeroizing::new(Vec::new());
    fs::File::open(path)?
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(invalid());
    }
    Ok(bytes)
}
pub(super) fn replace(
    directory: &Path,
    kind: CredentialKind,
    marker: &[u8; 16],
    payload: &[u8],
) -> Result<(), PersistenceError> {
    if payload.len() > maximum(kind) {
        return Err(invalid());
    }
    let temp = directory.join(temporary(kind, marker));
    let mut file = create_private_file(&temp)?;
    let result = (|| {
        file.write_all(payload)?;
        file.sync_all()?;
        drop(file);
        validate_private_file(&temp, Some(payload.len() as u64))?;
        fs::rename(&temp, path(directory, kind))?;
        sync_directory(directory)?;
        let installed = read(&path(directory, kind), maximum(kind))?;
        if installed.as_slice() != payload {
            return Err(invalid());
        }
        Ok(())
    })();
    if result.is_err() && path_entry_exists(&temp).unwrap_or(false) {
        let _ = fs::remove_file(&temp);
        let _ = sync_directory(directory);
    }
    result
}
