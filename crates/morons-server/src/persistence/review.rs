use super::{PersistenceError, ReviewResources, SessionId, SessionStore, WorkerRequest};
use hmac::{Hmac, KeyInit, Mac};
use morons_protocol::{DiffChange, DiffCursor, ReviewGeneration};
use sha2::Sha256;
use tokio::sync::oneshot;

const REVIEW_FORMAT_VERSION: u16 = 1;
const GENERATION_BYTES: usize = 98;
const CURSOR_PREFIX: &str = "dif1_";
const CURSOR_FIXED_BYTES: usize = 2 + 16 + 16 + 32 + 2 + 32;
const MAX_PATH_BYTES: usize = 4_096;
const MAX_PAGE_ENTRIES: u16 = 50;
const CURSOR_MAC_CONTEXT: &[u8] = b"morons.dev/diff-cursor/v1\0";
const GENERATION_MAC_CONTEXT: &[u8] = b"morons.dev/review-generation/v1\0";
type CursorMac = Hmac<Sha256>;

impl SessionStore {
    pub async fn review_diff(
        &self,
        session_id: SessionId,
        cursor: Option<DiffCursor>,
        limit: u16,
    ) -> Result<(Vec<DiffChange>, Option<DiffCursor>, ReviewGeneration), PersistenceError> {
        if limit == 0 || limit > MAX_PAGE_ENTRIES {
            return Err(PersistenceError::InvalidInput {
                reason: "diff page limit is invalid",
            });
        }
        let _workspace_guard = self.repository_import_lock.lock().await;
        let resources = self.review_resources(session_id).await?;
        let cursor_key = *self.review_cursor_key;
        let decoded = cursor
            .as_ref()
            .map(|cursor| decode_cursor(&cursor_key, cursor))
            .transpose()?
            .map(|cursor| {
                if cursor.session_id != *resources.session_id.as_bytes()
                    || cursor.generation_id != resources.generation_id
                {
                    return Err(PersistenceError::ReviewCursorStale);
                }
                Ok(cursor)
            })
            .transpose()?;
        let expected_manifest = decoded.as_ref().map(|cursor| cursor.active_manifest);
        let after = decoded.as_ref().map(|cursor| cursor.after_path.clone());
        let paths = self.paths.clone();
        let workspace_id = resources.workspace_id;
        let generation_id = resources.generation_id;
        let baseline_manifest = resources.baseline_manifest_digest;
        let scan_limit = limit.checked_add(1).ok_or(PersistenceError::InvalidInput {
            reason: "diff page limit overflowed",
        })?;
        let scan = tokio::task::spawn_blocking(move || {
            paths.review_diff(
                &workspace_id,
                &generation_id,
                &baseline_manifest,
                expected_manifest.as_ref(),
                after.as_deref(),
                scan_limit,
            )
        })
        .await
        .map_err(|_| PersistenceError::WorkerStopped)??;
        let generation = generation_token(&resources, &scan.active_manifest, &cursor_key);
        let mut changes = scan.changes;
        let next_cursor = if changes.len() > usize::from(limit) {
            changes.truncate(usize::from(limit));
            let after_path = changes
                .last()
                .ok_or(PersistenceError::InvalidState {
                    reason: "a diff page cursor has no preceding change",
                })?
                .path
                .clone();
            Some(encode_cursor(
                &resources,
                &scan.active_manifest,
                &after_path,
                &cursor_key,
            )?)
        } else {
            None
        };
        Ok((changes, next_cursor, generation))
    }

    async fn review_resources(
        &self,
        session_id: SessionId,
    ) -> Result<ReviewResources, PersistenceError> {
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::GetReviewResources {
                session_id,
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }
}

struct DecodedCursor {
    session_id: [u8; 16],
    generation_id: [u8; 16],
    active_manifest: [u8; 32],
    after_path: String,
}

fn generation_token(
    resources: &ReviewResources,
    active_manifest: &[u8; 32],
    cursor_key: &[u8; 32],
) -> ReviewGeneration {
    let mut bytes = [0_u8; GENERATION_BYTES];
    bytes[..2].copy_from_slice(&REVIEW_FORMAT_VERSION.to_be_bytes());
    bytes[2..18].copy_from_slice(resources.session_id.as_bytes());
    bytes[18..34].copy_from_slice(&resources.generation_id);
    bytes[34..66].copy_from_slice(active_manifest);
    let mut mac = CursorMac::new_from_slice(cursor_key).expect("HMAC-SHA256 accepts a 32-byte key");
    mac.update(GENERATION_MAC_CONTEXT);
    mac.update(&bytes[..66]);
    bytes[66..].copy_from_slice(&mac.finalize().into_bytes());
    ReviewGeneration::from_bytes(bytes)
}

fn encode_cursor(
    resources: &ReviewResources,
    active_manifest: &[u8; 32],
    after_path: &str,
    cursor_key: &[u8; 32],
) -> Result<DiffCursor, PersistenceError> {
    validate_cursor_path(after_path)?;
    let path_length =
        u16::try_from(after_path.len()).map_err(|_| PersistenceError::InvalidInput {
            reason: "diff cursor path is too long",
        })?;
    let mut bytes = Vec::with_capacity(CURSOR_FIXED_BYTES + after_path.len());
    bytes.extend_from_slice(&REVIEW_FORMAT_VERSION.to_be_bytes());
    bytes.extend_from_slice(resources.session_id.as_bytes());
    bytes.extend_from_slice(&resources.generation_id);
    bytes.extend_from_slice(active_manifest);
    bytes.extend_from_slice(&path_length.to_be_bytes());
    bytes.extend_from_slice(after_path.as_bytes());
    let digest = cursor_mac(cursor_key, &bytes).finalize().into_bytes();
    bytes.extend_from_slice(&digest);
    DiffCursor::from_token(format!("{CURSOR_PREFIX}{}", hex(&bytes))).ok_or(
        PersistenceError::InvalidState {
            reason: "a generated diff cursor exceeded its protocol bounds",
        },
    )
}

fn decode_cursor(
    cursor_key: &[u8; 32],
    cursor: &DiffCursor,
) -> Result<DecodedCursor, PersistenceError> {
    let token = cursor
        .as_token()
        .strip_prefix(CURSOR_PREFIX)
        .ok_or(PersistenceError::ReviewCursorStale)?;
    if token.len() % 2 != 0 {
        return Err(PersistenceError::ReviewCursorStale);
    }
    let bytes = decode_hex(token)?;
    if bytes.len() < CURSOR_FIXED_BYTES {
        return Err(PersistenceError::ReviewCursorStale);
    }
    let content_end = bytes.len() - 32;
    cursor_mac(cursor_key, &bytes[..content_end])
        .verify_slice(&bytes[content_end..])
        .map_err(|_| PersistenceError::ReviewCursorStale)?;
    let version = u16::from_be_bytes(
        bytes[..2]
            .try_into()
            .map_err(|_| PersistenceError::ReviewCursorStale)?,
    );
    if version != REVIEW_FORMAT_VERSION {
        return Err(PersistenceError::ReviewCursorStale);
    }
    let session_id = bytes[2..18]
        .try_into()
        .map_err(|_| PersistenceError::ReviewCursorStale)?;
    let generation_id = bytes[18..34]
        .try_into()
        .map_err(|_| PersistenceError::ReviewCursorStale)?;
    let active_manifest = bytes[34..66]
        .try_into()
        .map_err(|_| PersistenceError::ReviewCursorStale)?;
    let path_length = usize::from(u16::from_be_bytes(
        bytes[66..68]
            .try_into()
            .map_err(|_| PersistenceError::ReviewCursorStale)?,
    ));
    if 68_usize
        .checked_add(path_length)
        .is_none_or(|end| end != content_end)
    {
        return Err(PersistenceError::ReviewCursorStale);
    }
    let after_path = std::str::from_utf8(&bytes[68..content_end])
        .map_err(|_| PersistenceError::ReviewCursorStale)?
        .to_owned();
    validate_cursor_path(&after_path).map_err(|_| PersistenceError::ReviewCursorStale)?;
    Ok(DecodedCursor {
        session_id,
        generation_id,
        active_manifest,
        after_path,
    })
}

fn validate_cursor_path(path: &str) -> Result<(), PersistenceError> {
    if path.is_empty() || path.len() > MAX_PATH_BYTES || path.starts_with('/') {
        return Err(PersistenceError::InvalidInput {
            reason: "diff cursor path is invalid",
        });
    }
    if path
        .split('/')
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(PersistenceError::InvalidInput {
            reason: "diff cursor path is invalid",
        });
    }
    Ok(())
}

fn cursor_mac(key: &[u8; 32], bytes: &[u8]) -> CursorMac {
    let mut mac = CursorMac::new_from_slice(key).expect("HMAC-SHA256 accepts a 32-byte key");
    mac.update(CURSOR_MAC_CONTEXT);
    mac.update(bytes);
    mac
}

fn decode_hex(value: &str) -> Result<Vec<u8>, PersistenceError> {
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let text =
                std::str::from_utf8(pair).map_err(|_| PersistenceError::ReviewCursorStale)?;
            u8::from_str_radix(text, 16).map_err(|_| PersistenceError::ReviewCursorStale)
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
