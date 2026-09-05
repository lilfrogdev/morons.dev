use rusqlite::params;

use super::{Backend, records::sequence_to_sql, run_records::transcript_entry_from_row};
use crate::persistence::{
    PersistenceError, SessionId, TranscriptEntry, compactions::ContextSourceHasher,
};

const PAGE_ENTRIES: u16 = 32;

impl Backend {
    /// Bounded canonical pages, including hidden commands for source hashing only.
    pub(super) fn visit_context_entries(
        &self,
        session_id: SessionId,
        after: u64,
        through: u64,
        mut visit: impl FnMut(TranscriptEntry) -> Result<bool, PersistenceError>,
    ) -> Result<(), PersistenceError> {
        let event_high_water =
            super::session_events::session_event_high_water(&self.connection, session_id)?;
        let mut after = after;
        let mut statement = self.connection.prepare(
            "SELECT entry.entry_sequence, entry.message_id, entry.run_id, entry.entry_kind,
                    entry.open_code_service, entry.model_id, entry.text, entry.refusal,
                    entry.created_at_milliseconds, entry.assistant_phase, entry.tool_call_id,
                    call.operation_id, call.provider_operation_id, call.input_payload, result.result_payload
             FROM session_entries AS entry
             LEFT JOIN tool_calls AS call ON call.call_id = entry.tool_call_id
             LEFT JOIN tool_operation_facts AS result
               ON result.call_id = entry.tool_call_id AND result.fact_kind BETWEEN 3 AND 6
             WHERE entry.session_id = ?1 AND entry.entry_sequence > ?2 AND entry.entry_sequence <= ?3
             ORDER BY entry.entry_sequence LIMIT ?4",
        )?;
        while after < through {
            let mut entries = statement
                .query_map(
                    params![
                        &session_id.as_bytes()[..],
                        sequence_to_sql(after)?,
                        sequence_to_sql(through)?,
                        PAGE_ENTRIES
                    ],
                    transcript_entry_from_row,
                )?
                .collect::<Result<Vec<_>, _>>()?;
            entries.extend(self.list_local_command_entries(
                session_id,
                after,
                through,
                event_high_water,
                PAGE_ENTRIES,
            )?);
            entries.sort_by_key(TranscriptEntry::entry_sequence);
            entries.truncate(usize::from(PAGE_ENTRIES));
            self.attach_image_metadata(session_id, &mut entries)?;
            if entries.is_empty() {
                return Err(invalid_prefix());
            }
            for entry in entries {
                if entry.entry_sequence() != after + 1 {
                    return Err(invalid_prefix());
                }
                after = entry.entry_sequence();
                if !visit(entry)? {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    pub(super) fn context_digest_through(
        &self,
        session_id: SessionId,
        high_water: u64,
    ) -> Result<[u8; 32], PersistenceError> {
        let mut digest = ContextSourceHasher::new(high_water);
        self.visit_context_entries(session_id, 0, high_water, |entry| {
            digest.push(&entry).ok_or_else(invalid_prefix)?;
            Ok(true)
        })?;
        digest.finish().ok_or_else(invalid_prefix)
    }

    /// Other connections cannot silently invalidate the startup integrity proof.
    /// Our sole worker's writes are validated at their service/commit boundaries.
    pub(super) fn ensure_context_integrity(&self) -> Result<(), PersistenceError> {
        let version: i64 = self
            .connection
            .query_row("PRAGMA data_version", [], |row| row.get(0))?;
        if self.context_data_version.get() != Some(version) {
            self.validate_context_checkpoint_digests()?;
            self.context_data_version.set(Some(version));
        }
        Ok(())
    }
}

fn invalid_prefix() -> PersistenceError {
    PersistenceError::InvalidState {
        reason: "a canonical context prefix is not contiguous",
    }
}
