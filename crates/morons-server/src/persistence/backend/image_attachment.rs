use std::{
    collections::BTreeSet,
    fs,
    io::{Read as _, Write as _},
};

use rusqlite::OptionalExtension as _;
use sha2::{Digest, Sha256};

use super::records::random_identifier;
use crate::persistence::{
    ImageAttachment, ImageAttachmentId, PersistenceError, PreparedImageAttachment, SessionId,
    images::{media_type_from_record, valid_stored_attachments},
    paths::StoragePaths,
};

pub(super) struct AttachmentStaging {
    paths: StoragePaths,
    session_id: SessionId,
    attachments: Vec<ImageAttachment>,
    committed: bool,
}

impl AttachmentStaging {
    pub(super) fn stage(
        paths: StoragePaths,
        session_id: SessionId,
        prepared: &[PreparedImageAttachment],
    ) -> Result<Self, PersistenceError> {
        let mut staging = Self {
            paths,
            session_id,
            attachments: Vec::with_capacity(prepared.len()),
            committed: false,
        };
        for attachment in prepared {
            let id = ImageAttachmentId::from_bytes(random_identifier()?);
            let (_, mut file) = staging
                .paths
                .create_attachment_file(session_id.as_bytes(), id.as_bytes())?;
            staging.attachments.push(ImageAttachment {
                id,
                display_name: attachment.display_name.clone(),
                marker_start: attachment.marker_start,
                media_type: attachment.media_type,
                width: attachment.width,
                height: attachment.height,
                bytes: u64::try_from(attachment.bytes.len()).map_err(|_| {
                    PersistenceError::InvalidInput {
                        reason: "an image attachment byte count is invalid",
                    }
                })?,
                digest: attachment.digest,
            });
            file.write_all(&attachment.bytes)?;
            file.sync_all()?;
        }
        if !prepared.is_empty() {
            staging
                .paths
                .sync_attachment_session_directory(session_id.as_bytes())?;
        }
        Ok(staging)
    }

    pub(super) fn attachments(&self) -> &[ImageAttachment] {
        &self.attachments
    }

    pub(super) fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for AttachmentStaging {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for attachment in &self.attachments {
            let _ = self
                .paths
                .remove_attachment_file(self.session_id.as_bytes(), attachment.id.as_bytes());
        }
    }
}

impl super::Backend {
    pub(super) fn reconcile_image_attachments(&self) -> Result<(), PersistenceError> {
        let attachments = self.load_all_image_attachments()?;
        let tool_attachments = self.load_all_tool_image_attachments()?;
        let stored_ids = attachments
            .iter()
            .map(|(session_id, _, attachment)| (*session_id.as_bytes(), *attachment.id.as_bytes()))
            .chain(tool_attachments.iter().map(|(_, session_id, attachment)| {
                (*session_id.as_bytes(), *attachment.id.as_bytes())
            }))
            .collect::<BTreeSet<_>>();
        if stored_ids.len() != attachments.len() + tool_attachments.len() {
            return Err(PersistenceError::InvalidState {
                reason: "durable image attachment identifiers are duplicated",
            });
        }
        for (session_id, file_id) in self.paths.attachment_file_ids()? {
            if !stored_ids.contains(&(session_id, file_id)) {
                self.paths.remove_attachment_file(&session_id, &file_id)?;
            }
        }
        for (session_id, attachment) in attachments
            .into_iter()
            .map(|(session_id, _, attachment)| (session_id, attachment))
            .chain(
                tool_attachments
                    .into_iter()
                    .map(|(_, session_id, attachment)| (session_id, attachment)),
            )
        {
            let bytes = self.read_image_attachment(session_id, attachment.id)?;
            if bytes.len() as u64 != attachment.bytes
                || Sha256::digest(&bytes)[..] != attachment.digest
                || !morons_image::validate_normalized_image(
                    &bytes,
                    attachment.media_type,
                    attachment.width,
                    attachment.height,
                )
            {
                return Err(PersistenceError::InvalidState {
                    reason: "a durable image attachment is invalid",
                });
            }
        }
        Ok(())
    }

    pub(super) fn read_image_attachment(
        &self,
        session_id: SessionId,
        attachment_id: ImageAttachmentId,
    ) -> Result<Vec<u8>, PersistenceError> {
        let path = self.paths.validate_attachment_file(
            session_id.as_bytes(),
            attachment_id.as_bytes(),
            morons_image::MAX_NORMALIZED_IMAGE_BYTES as u64,
        )?;
        let mut file = fs::File::open(path)?;
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take(morons_image::MAX_NORMALIZED_IMAGE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > morons_image::MAX_NORMALIZED_IMAGE_BYTES {
            return Err(PersistenceError::InvalidState {
                reason: "a durable image attachment exceeds its byte limit",
            });
        }
        Ok(bytes)
    }

    pub(super) fn load_message_image_attachments(
        &self,
        session_id: SessionId,
        user_message_id: crate::persistence::MessageId,
    ) -> Result<Vec<ImageAttachment>, PersistenceError> {
        load_message_image_attachments(&self.connection, session_id, user_message_id)
    }

    fn load_all_tool_image_attachments(
        &self,
    ) -> Result<Vec<(crate::persistence::ToolCallId, SessionId, ImageAttachment)>, PersistenceError>
    {
        let mut statement = self.connection.prepare(
            "SELECT call_id, session_id, attachment_id, display_name, 0, media_type,
                    width, height, byte_count, sha256
             FROM tool_image_attachments ORDER BY run_id, call_id",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    crate::persistence::ToolCallId::from_bytes(row.get(0)?),
                    SessionId::from_bytes(row.get(1)?),
                    image_attachment_from_row_offset(row, 2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(PersistenceError::from)
    }

    fn load_all_image_attachments(
        &self,
    ) -> Result<Vec<(SessionId, crate::persistence::MessageId, ImageAttachment)>, PersistenceError>
    {
        let mut statement = self.connection.prepare(
            "SELECT session_id, user_message_id, attachment_id, display_name,
                    marker_start, media_type, width, height, byte_count, sha256
             FROM image_attachments ORDER BY run_id, attachment_index",
        )?;
        let rows = statement
            .query_map([], |row| {
                let session_id = SessionId::from_bytes(row.get(0)?);
                let message_id = crate::persistence::MessageId::from_bytes(row.get(1)?);
                let media_type = media_type_from_record(row.get(5)?).ok_or_else(|| {
                    rusqlite::Error::InvalidColumnType(
                        5,
                        "media_type".to_owned(),
                        rusqlite::types::Type::Integer,
                    )
                })?;
                let width = positive_u32(row.get(6)?, 6, "width")?;
                let height = positive_u32(row.get(7)?, 7, "height")?;
                let bytes = positive_u64(row.get(8)?, 8, "byte_count")?;
                Ok((
                    session_id,
                    message_id,
                    ImageAttachment {
                        id: ImageAttachmentId::from_bytes(row.get(2)?),
                        display_name: row.get(3)?,
                        marker_start: nonnegative_u32(row.get(4)?, 4, "marker_start")?,
                        media_type,
                        width,
                        height,
                        bytes,
                        digest: row.get(9)?,
                    },
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut start = 0_usize;
        while start < rows.len() {
            let (session_id, message_id, _) = &rows[start];
            let mut end = start + 1;
            while end < rows.len() && rows[end].0 == *session_id && rows[end].1 == *message_id {
                end += 1;
            }
            let text: String = self.connection.query_row(
                "SELECT text FROM session_entries
                 WHERE session_id = ?1 AND message_id = ?2 AND entry_kind = 1",
                rusqlite::params![&session_id.as_bytes()[..], &message_id.as_bytes()[..]],
                |row| row.get(0),
            )?;
            let attachments = rows[start..end]
                .iter()
                .map(|(_, _, attachment)| attachment.clone())
                .collect::<Vec<_>>();
            if !valid_stored_attachments(&text, &attachments) {
                return Err(PersistenceError::InvalidState {
                    reason: "durable image attachment metadata is invalid",
                });
            }
            start = end;
        }
        Ok(rows)
    }
}

pub(crate) fn load_tool_image_attachment(
    connection: &rusqlite::Connection,
    call_id: crate::persistence::ToolCallId,
) -> Result<Option<ImageAttachment>, PersistenceError> {
    connection
        .query_row(
            "SELECT attachment_id, display_name, 0, media_type,
                    width, height, byte_count, sha256
             FROM tool_image_attachments WHERE call_id = ?1",
            [&call_id.as_bytes()[..]],
            image_attachment_from_row,
        )
        .optional()
        .map_err(PersistenceError::from)
}

pub(crate) fn load_message_image_attachments(
    connection: &rusqlite::Connection,
    session_id: SessionId,
    user_message_id: crate::persistence::MessageId,
) -> Result<Vec<ImageAttachment>, PersistenceError> {
    let mut statement = connection.prepare(
        "SELECT attachment_id, display_name, marker_start, media_type,
                width, height, byte_count, sha256
         FROM image_attachments
         WHERE session_id = ?1 AND user_message_id = ?2
         ORDER BY attachment_index",
    )?;
    let attachments = statement
        .query_map(
            rusqlite::params![&session_id.as_bytes()[..], &user_message_id.as_bytes()[..]],
            image_attachment_from_row,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(attachments)
}

pub(super) fn image_attachment_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ImageAttachment> {
    image_attachment_from_row_offset(row, 0)
}

fn image_attachment_from_row_offset(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<ImageAttachment> {
    let media_type = media_type_from_record(row.get(offset + 3)?).ok_or_else(|| {
        rusqlite::Error::InvalidColumnType(
            offset + 3,
            "media_type".to_owned(),
            rusqlite::types::Type::Integer,
        )
    })?;
    Ok(ImageAttachment {
        id: ImageAttachmentId::from_bytes(row.get(offset)?),
        display_name: row.get(offset + 1)?,
        marker_start: nonnegative_u32(row.get(offset + 2)?, offset + 2, "marker_start")?,
        media_type,
        width: positive_u32(row.get(offset + 4)?, offset + 4, "width")?,
        height: positive_u32(row.get(offset + 5)?, offset + 5, "height")?,
        bytes: positive_u64(row.get(offset + 6)?, offset + 6, "byte_count")?,
        digest: row.get(offset + 7)?,
    })
}

fn positive_u32(value: i64, index: usize, name: &str) -> rusqlite::Result<u32> {
    u32::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            rusqlite::Error::InvalidColumnType(
                index,
                name.to_owned(),
                rusqlite::types::Type::Integer,
            )
        })
}

fn nonnegative_u32(value: i64, index: usize, name: &str) -> rusqlite::Result<u32> {
    u32::try_from(value).map_err(|_| {
        rusqlite::Error::InvalidColumnType(index, name.to_owned(), rusqlite::types::Type::Integer)
    })
}

fn positive_u64(value: i64, index: usize, name: &str) -> rusqlite::Result<u64> {
    u64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            rusqlite::Error::InvalidColumnType(
                index,
                name.to_owned(),
                rusqlite::types::Type::Integer,
            )
        })
}
