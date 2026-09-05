use rusqlite::{Connection, params};

use super::{Backend, records::sequence_to_sql};
use crate::persistence::{
    PersistenceError, SessionId,
    images::{MAX_CONTEXT_IMAGE_BYTES, MAX_CONTEXT_IMAGES},
};

pub(super) const CONTEXT_REQUEST_RESERVE: u64 = 8_192;
pub(super) const CONTEXT_ITEM_RESERVE: u64 = 24;
pub(super) const MAX_ACTIVE_CONTEXT_ENTRIES: usize = 256;
pub(super) const MAX_COMPACTION_SUMMARY_BYTES: usize = 16 * 1024;

#[derive(Default)]
pub(super) struct ContextBudget {
    pub entries: u64,
    pub bytes: u64,
    pub images: u64,
    pub image_bytes: u64,
    // Advisory only; fits() always uses the independent conservative guard.
    pub observed_input_tokens: Option<u64>,
}

impl ContextBudget {
    pub(super) fn tokens(&self, extra_bytes: usize) -> u64 {
        self.bytes
            .saturating_add(extra_bytes as u64)
            .saturating_add(self.entries.saturating_mul(16))
            .saturating_add(self.images.saturating_mul(8_192))
            .saturating_add(CONTEXT_REQUEST_RESERVE)
    }

    pub(super) fn estimated_tokens(&self, extra_bytes: usize) -> u64 {
        self.observed_input_tokens
            .unwrap_or_else(|| self.tokens(extra_bytes))
    }

    pub(super) fn pressure(&self, maximum_input_tokens: u32, extra_bytes: usize) -> bool {
        !self.fits(maximum_input_tokens, extra_bytes)
            || self.estimated_tokens(extra_bytes) >= u64::from(maximum_input_tokens) * 7 / 10
            || self
                .entries
                .saturating_add(self.images)
                .saturating_add(CONTEXT_ITEM_RESERVE)
                >= 192
            || self.images >= MAX_CONTEXT_IMAGES as u64 * 3 / 4
            || self.image_bytes >= MAX_CONTEXT_IMAGE_BYTES * 3 / 4
    }

    pub(super) fn fits(&self, maximum_input_tokens: u32, extra_bytes: usize) -> bool {
        self.tokens(extra_bytes) <= u64::from(maximum_input_tokens)
            && self
                .entries
                .saturating_add(self.images)
                .saturating_add(CONTEXT_ITEM_RESERVE)
                <= MAX_ACTIVE_CONTEXT_ENTRIES as u64
            && self.images <= MAX_CONTEXT_IMAGES as u64
            && self.image_bytes <= MAX_CONTEXT_IMAGE_BYTES
    }
}

impl Backend {
    pub(super) fn context_budget(
        &self,
        session: SessionId,
        after: u64,
        through: u64,
    ) -> Result<ContextBudget, PersistenceError> {
        context_budget(&self.connection, session, after, through)
    }
}

pub(super) fn context_budget(
    connection: &Connection,
    session: SessionId,
    after: u64,
    through: u64,
) -> Result<ContextBudget, PersistenceError> {
    let parameters = params![
        &session.as_bytes()[..],
        sequence_to_sql(after)?,
        sequence_to_sql(through)?
    ];
    let (entries, bytes): (i64, i64) = connection.query_row(
        "SELECT COUNT(*), COALESCE(SUM(bytes), 0) FROM (
            SELECT COALESCE(length(CAST(entry.text AS BLOB)), length(call.input_payload), length(result.result_payload), 0) AS bytes
            FROM session_entries AS entry
            LEFT JOIN tool_calls AS call ON call.call_id = entry.tool_call_id
            LEFT JOIN tool_operation_facts AS result ON result.call_id = entry.tool_call_id AND result.fact_kind BETWEEN 3 AND 6
            WHERE entry.session_id = ?1 AND entry.entry_sequence > ?2 AND entry.entry_sequence <= ?3
            UNION ALL
            SELECT length(CAST(command_text AS BLOB)) + length(result_payload)
            FROM local_commands WHERE session_id = ?1 AND entry_sequence > ?2 AND entry_sequence <= ?3
                AND context_visible = 1 AND state BETWEEN 3 AND 5
        )", parameters, |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let (images, image_bytes): (i64, i64) = connection.query_row(
        "SELECT COUNT(*), COALESCE(SUM(byte_count), 0) FROM (
            SELECT attachment.byte_count FROM image_attachments AS attachment
            JOIN session_entries AS entry ON entry.message_id = attachment.user_message_id
            WHERE attachment.session_id = ?1 AND entry.entry_sequence > ?2 AND entry.entry_sequence <= ?3
            UNION ALL
            SELECT attachment.byte_count FROM tool_image_attachments AS attachment
            JOIN session_entries AS entry ON entry.tool_call_id = attachment.call_id AND entry.entry_kind = 4
            WHERE attachment.session_id = ?1 AND entry.entry_sequence > ?2 AND entry.entry_sequence <= ?3
        )", parameters, |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(ContextBudget {
        entries: nonnegative(entries)?,
        bytes: nonnegative(bytes)?,
        images: nonnegative(images)?,
        image_bytes: nonnegative(image_bytes)?,
        observed_input_tokens: None,
    })
}

fn nonnegative(value: i64) -> Result<u64, PersistenceError> {
    u64::try_from(value).map_err(|_| PersistenceError::InvalidState {
        reason: "a context budget is negative",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_context_budgets_trigger_before_hard_limits() {
        for budget in [
            ContextBudget {
                entries: 168,
                ..ContextBudget::default()
            },
            ContextBudget {
                images: 12,
                ..ContextBudget::default()
            },
            ContextBudget {
                images: 1,
                image_bytes: MAX_CONTEXT_IMAGE_BYTES * 3 / 4,
                ..ContextBudget::default()
            },
            ContextBudget {
                bytes: 67_200 - CONTEXT_REQUEST_RESERVE,
                ..ContextBudget::default()
            },
        ] {
            assert!(budget.pressure(96_000, 0));
        }
        assert!(
            !ContextBudget {
                entries: 250,
                ..ContextBudget::default()
            }
            .fits(96_000, 0)
        );
        assert!(
            !ContextBudget {
                images: 17,
                ..ContextBudget::default()
            }
            .fits(96_000, 0)
        );
        assert!(
            !ContextBudget {
                image_bytes: MAX_CONTEXT_IMAGE_BYTES + 1,
                ..ContextBudget::default()
            }
            .fits(96_000, 0)
        );
        assert!(
            !ContextBudget {
                bytes: 95_000,
                ..ContextBudget::default()
            }
            .fits(96_000, 0)
        );
    }
}
