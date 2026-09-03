use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use super::{ImageAttachment, PreparedImageAttachment, SessionId};

pub(crate) const MAX_IMAGE_ATTACHMENTS_PER_MESSAGE: usize = 4;
pub(crate) const MAX_IMAGE_DISPLAY_NAME_BYTES: usize = 128;
pub(crate) const MAX_IMAGE_ATTACHMENT_AGGREGATE_BYTES: usize = 6 * 1024 * 1024;
pub(crate) const MAX_ATTACHMENT_STORAGE_BYTES: u64 = 1024 * 1024 * 1024;
pub(crate) const MAX_CONTEXT_IMAGES: usize = 16;
pub(crate) const MAX_CONTEXT_IMAGE_BYTES: u64 = 6 * 1024 * 1024;
const ATTACHMENT_BINDING_CONTEXT: &[u8] = b"morons.dev/image-attachment-binding/v1\0";

pub(crate) fn validate_prepared_attachments(
    text: &str,
    attachments: &[PreparedImageAttachment],
) -> bool {
    if attachments.len() > MAX_IMAGE_ATTACHMENTS_PER_MESSAGE {
        return false;
    }
    let mut names = BTreeSet::new();
    let mut last_end = 0_usize;
    let mut aggregate_bytes = 0_usize;
    for attachment in attachments {
        if !valid_display_name(&attachment.display_name)
            || !names.insert(attachment.display_name.as_str())
            || attachment.width == 0
            || attachment.height == 0
            || attachment.width > morons_image::MAX_IMAGE_DIMENSION
            || attachment.height > morons_image::MAX_IMAGE_DIMENSION
            || attachment.bytes.is_empty()
            || attachment.bytes.len() > morons_image::MAX_NORMALIZED_IMAGE_BYTES
            || attachment.digest != Sha256::digest(&attachment.bytes)[..]
        {
            return false;
        }
        let marker = format!("[{}]", attachment.display_name);
        let Ok(start) = usize::try_from(attachment.marker_start) else {
            return false;
        };
        let Some(end) = start.checked_add(marker.len()) else {
            return false;
        };
        if start < last_end
            || end > text.len()
            || !text.is_char_boundary(start)
            || !text.is_char_boundary(end)
            || text.get(start..end) != Some(marker.as_str())
        {
            return false;
        }
        last_end = end;
        let Some(bytes) = aggregate_bytes.checked_add(attachment.bytes.len()) else {
            return false;
        };
        aggregate_bytes = bytes;
        if !morons_image::validate_normalized_image(
            &attachment.bytes,
            attachment.media_type,
            attachment.width,
            attachment.height,
        ) {
            return false;
        }
    }
    aggregate_bytes <= MAX_IMAGE_ATTACHMENT_AGGREGATE_BYTES
}

pub(crate) fn valid_stored_attachments(text: &str, attachments: &[ImageAttachment]) -> bool {
    if attachments.len() > MAX_IMAGE_ATTACHMENTS_PER_MESSAGE {
        return false;
    }
    let mut names = BTreeSet::new();
    let mut identifiers = BTreeSet::new();
    let mut last_end = 0_usize;
    let mut aggregate_bytes = 0_u64;
    for attachment in attachments {
        if !attachment.id.as_bytes().iter().any(|byte| *byte != 0)
            || !identifiers.insert(*attachment.id.as_bytes())
            || !valid_display_name(&attachment.display_name)
            || !names.insert(attachment.display_name.as_str())
            || attachment.width == 0
            || attachment.height == 0
            || attachment.width > morons_image::MAX_IMAGE_DIMENSION
            || attachment.height > morons_image::MAX_IMAGE_DIMENSION
            || attachment.bytes == 0
            || attachment.bytes > morons_image::MAX_NORMALIZED_IMAGE_BYTES as u64
        {
            return false;
        }
        let marker = format!("[{}]", attachment.display_name);
        let Ok(start) = usize::try_from(attachment.marker_start) else {
            return false;
        };
        let Some(end) = start.checked_add(marker.len()) else {
            return false;
        };
        if start < last_end
            || end > text.len()
            || !text.is_char_boundary(start)
            || !text.is_char_boundary(end)
            || text.get(start..end) != Some(marker.as_str())
        {
            return false;
        }
        last_end = end;
        let Some(bytes) = aggregate_bytes.checked_add(attachment.bytes) else {
            return false;
        };
        aggregate_bytes = bytes;
    }
    aggregate_bytes <= MAX_IMAGE_ATTACHMENT_AGGREGATE_BYTES as u64
}

pub(crate) fn prepared_attachment_digest(attachments: &[PreparedImageAttachment]) -> [u8; 32] {
    attachment_digest(attachments.iter().map(|attachment| AttachmentDigestPart {
        display_name: &attachment.display_name,
        marker_start: attachment.marker_start,
        media_type: attachment.media_type,
        width: attachment.width,
        height: attachment.height,
        bytes: attachment.bytes.len() as u64,
        digest: &attachment.digest,
    }))
}

pub(crate) fn stored_attachment_digest(attachments: &[ImageAttachment]) -> [u8; 32] {
    attachment_digest(attachments.iter().map(|attachment| AttachmentDigestPart {
        display_name: &attachment.display_name,
        marker_start: attachment.marker_start,
        media_type: attachment.media_type,
        width: attachment.width,
        height: attachment.height,
        bytes: attachment.bytes,
        digest: &attachment.digest,
    }))
}

fn attachment_digest<'a>(parts: impl Iterator<Item = AttachmentDigestPart<'a>>) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(ATTACHMENT_BINDING_CONTEXT);
    for part in parts {
        digest.update((part.display_name.len() as u32).to_be_bytes());
        digest.update(part.display_name.as_bytes());
        digest.update(part.marker_start.to_be_bytes());
        digest.update([media_type_record(part.media_type)]);
        digest.update(part.width.to_be_bytes());
        digest.update(part.height.to_be_bytes());
        digest.update(part.bytes.to_be_bytes());
        digest.update(part.digest);
    }
    digest.finalize().into()
}

struct AttachmentDigestPart<'a> {
    display_name: &'a str,
    marker_start: u32,
    media_type: morons_image::ImageMediaType,
    width: u32,
    height: u32,
    bytes: u64,
    digest: &'a [u8; 32],
}

pub(crate) const fn media_type_record(media_type: morons_image::ImageMediaType) -> u8 {
    match media_type {
        morons_image::ImageMediaType::Png => 1,
        morons_image::ImageMediaType::Jpeg => 2,
        morons_image::ImageMediaType::Gif => 3,
    }
}

pub(crate) const fn media_type_from_record(value: i64) -> Option<morons_image::ImageMediaType> {
    match value {
        1 => Some(morons_image::ImageMediaType::Png),
        2 => Some(morons_image::ImageMediaType::Jpeg),
        3 => Some(morons_image::ImageMediaType::Gif),
        _ => None,
    }
}

pub(crate) fn image_context_capacity_available(
    connection: &rusqlite::Connection,
    session_id: SessionId,
    additional_count: usize,
    additional_bytes: u64,
) -> Result<bool, rusqlite::Error> {
    let (stored_count, stored_bytes): (i64, i64) = connection.query_row(
        "WITH checkpoint(high_water) AS (
            SELECT COALESCE(MAX(source_entry_high_water), 0)
            FROM context_checkpoints WHERE session_id = ?1
         ), visible(byte_count) AS (
            SELECT attachment.byte_count
            FROM image_attachments AS attachment
            JOIN session_entries AS entry ON entry.message_id = attachment.user_message_id
            WHERE attachment.session_id = ?1
              AND entry.entry_sequence > (SELECT high_water FROM checkpoint)
            UNION ALL
            SELECT attachment.byte_count
            FROM tool_image_attachments AS attachment
            JOIN session_entries AS entry ON entry.tool_call_id = attachment.call_id
            WHERE attachment.session_id = ?1 AND entry.entry_kind = 4
              AND entry.entry_sequence > (SELECT high_water FROM checkpoint)
         )
         SELECT COUNT(*), COALESCE(SUM(byte_count), 0) FROM visible",
        [&session_id.as_bytes()[..]],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(usize::try_from(stored_count)
        .ok()
        .and_then(|count| count.checked_add(additional_count))
        .is_some_and(|count| count <= MAX_CONTEXT_IMAGES)
        && u64::try_from(stored_bytes)
            .ok()
            .and_then(|bytes| bytes.checked_add(additional_bytes))
            .is_some_and(|bytes| bytes <= MAX_CONTEXT_IMAGE_BYTES))
}

pub(crate) fn attachment_storage_available(
    connection: &rusqlite::Connection,
    additional_bytes: u64,
) -> Result<bool, rusqlite::Error> {
    let stored: i64 = connection.query_row(
        "SELECT
            COALESCE((SELECT SUM(byte_count) FROM image_attachments), 0) +
            COALESCE((SELECT SUM(byte_count) FROM tool_image_attachments), 0)",
        [],
        |row| row.get(0),
    )?;
    Ok(u64::try_from(stored)
        .ok()
        .and_then(|stored| stored.checked_add(additional_bytes))
        .is_some_and(|bytes| bytes <= MAX_ATTACHMENT_STORAGE_BYTES))
}

pub(crate) fn valid_display_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IMAGE_DISPLAY_NAME_BYTES
        && value != "."
        && value != ".."
        && !value
            .chars()
            .any(|character| character.is_control() || is_bidirectional_control(character))
        && !value.contains(['/', '\\', '[', ']'])
}

const fn is_bidirectional_control(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}
