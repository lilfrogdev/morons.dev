use std::collections::VecDeque;

use rusqlite::params;

use super::{Backend, context_budget::MAX_COMPACTION_SUMMARY_BYTES, records::sequence_to_sql};
use crate::persistence::{
    CompactionPlan, ContextCheckpoint, PersistenceError, Run, SessionId, TranscriptEntry,
    compactions::ContextSourceHasher,
};

const MAX_SOURCE_BYTES: usize = 48 * 1024;
const MAX_ENTRY_EXCERPT_BYTES: usize = 8 * 1024;
const MAX_GUIDANCE_BYTES: usize = 8 * 1024;
const OMITTED: &str = "[Earlier context-visible source omitted to fit the summarization budget. The canonical source remains intact.]\n";

impl Backend {
    pub(super) fn plan_context_compaction(
        &self,
        run: &Run,
        checkpoint: Option<&ContextCheckpoint>,
        through: u64,
        instruction_bytes: usize,
        budget: &super::context_budget::ContextBudget,
    ) -> Result<Option<CompactionPlan>, PersistenceError> {
        // Never repeat a completed or uncertain compaction in the same run.
        let already_compacted: bool = self.connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM compaction_operations WHERE run_id = ?1)",
            [&run.id.as_bytes()[..]],
            |row| row.get(0),
        )?;
        if already_compacted {
            return Ok(None);
        }
        let prompt: String = self.connection.query_row(
            "SELECT text FROM session_entries WHERE run_id = ?1 AND entry_kind = 1",
            [&run.id.as_bytes()[..]],
            |row| row.get(0),
        )?;
        let manual = prompt == "/compact" || prompt.starts_with("/compact ");
        let covered = checkpoint.map_or(0, |checkpoint| checkpoint.source_entry_high_water);
        if !manual
            && !budget.pressure(
                run.maximum_input_tokens,
                instruction_bytes + checkpoint.map_or(0, |checkpoint| checkpoint.summary.len()),
            )
        {
            return Ok(None);
        }
        let Some(source_entry_high_water) = self.select_compaction_prefix(
            run.session_id,
            covered,
            run.source_entry_high_water,
            through,
            run.maximum_input_tokens,
            instruction_bytes,
        )?
        else {
            return Ok(None);
        };
        if !manual
            && self.compaction_prefix_was_attempted(run.session_id, source_entry_high_water)?
        {
            return Ok(None);
        }
        self.project_compaction_prefix(
            run.session_id,
            checkpoint,
            source_entry_high_water,
            prompt.strip_prefix("/compact "),
        )
        .map(Some)
    }

    /// Select a whole-turn cut inside a caller-validated window, not permission to dispatch.
    pub(super) fn select_compaction_prefix(
        &self,
        session_id: SessionId,
        covered: u64,
        protected_from: u64,
        through: u64,
        maximum_input_tokens: u32,
        instruction_bytes: usize,
    ) -> Result<Option<u64>, PersistenceError> {
        self.select_compaction_prefix_fitting(
            session_id,
            covered,
            protected_from,
            through,
            |tail| {
                tail.fits(
                    maximum_input_tokens,
                    instruction_bytes + MAX_COMPACTION_SUMMARY_BYTES,
                )
            },
        )
    }

    pub(super) fn select_compaction_prefix_fitting(
        &self,
        session_id: SessionId,
        covered: u64,
        protected_from: u64,
        through: u64,
        accepts_tail: impl Fn(&super::context_budget::ContextBudget) -> bool,
    ) -> Result<Option<u64>, PersistenceError> {
        if protected_from <= covered.saturating_add(1) || protected_from > through {
            return Ok(None);
        }
        // Retain two prior turns, then one, then at least the protected turn.
        let mut statement = self.connection.prepare(
            "SELECT entry_sequence FROM session_entries WHERE session_id = ?1 AND entry_kind = 1
             AND entry_sequence > ?2 AND entry_sequence < ?3 ORDER BY entry_sequence DESC LIMIT 2",
        )?;
        let mut boundaries = statement
            .query_map(
                params![
                    &session_id.as_bytes()[..],
                    sequence_to_sql(covered)?,
                    sequence_to_sql(protected_from)?
                ],
                |row| {
                    let value: i64 = row.get(0)?;
                    u64::try_from(value)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        boundaries.reverse();
        boundaries.push(protected_from);
        for boundary in boundaries {
            let high_water = boundary - 1;
            if high_water > covered
                && accepts_tail(&self.context_budget(session_id, high_water, through)?)
            {
                return Ok(Some(high_water));
            }
        }
        Ok(None)
    }

    /// Project a fixed prefix; callers own lifecycle eligibility and checkpoint validation.
    pub(super) fn project_compaction_prefix(
        &self,
        session_id: SessionId,
        checkpoint: Option<&ContextCheckpoint>,
        source_entry_high_water: u64,
        user_guidance: Option<&str>,
    ) -> Result<CompactionPlan, PersistenceError> {
        let covered = checkpoint.map_or(0, |checkpoint| checkpoint.source_entry_high_water);
        if source_entry_high_water <= covered {
            return Err(PersistenceError::InvalidState {
                reason: "a compaction prefix does not advance beyond its parent checkpoint",
            });
        }
        let mut digest = ContextSourceHasher::new(source_entry_high_water);
        let mut excerpts = VecDeque::new();
        let mut excerpt_bytes = 0;
        let mut omitted = false;
        self.visit_context_entries(session_id, 0, source_entry_high_water, |entry| {
            digest.push(&entry).ok_or(PersistenceError::InvalidState {
                reason: "a compaction source prefix is not contiguous",
            })?;
            if entry.entry_sequence() <= covered {
                return Ok(true);
            }
            let Some(text) = project_entry(&entry)? else {
                return Ok(true);
            };
            let excerpt = bounded_excerpt(&text, MAX_ENTRY_EXCERPT_BYTES);
            excerpt_bytes += excerpt.len();
            excerpts.push_back(excerpt);
            while excerpt_bytes > MAX_SOURCE_BYTES {
                let oldest = excerpts.pop_front().expect("an over-budget excerpt exists");
                excerpt_bytes -= oldest.len();
                omitted = true;
            }
            Ok(true)
        })?;
        let mut source = String::new();
        if omitted {
            source.push_str(OMITTED);
        }
        for excerpt in excerpts {
            source.push_str(&excerpt);
        }
        if source.is_empty() {
            source.push_str("[No context-visible source text in this prefix.]\n");
        }
        let parent_summary = checkpoint
            .map(|checkpoint| bounded_excerpt(&checkpoint.summary, MAX_COMPACTION_SUMMARY_BYTES));
        let estimated_input_tokens =
            u32::try_from(source.len() + parent_summary.as_ref().map_or(0, String::len) + 32)
                .map_err(|_| PersistenceError::InvalidState {
                    reason: "a bounded compaction source estimate overflowed",
                })?;
        Ok(CompactionPlan {
            parent_checkpoint_id: checkpoint.map(|checkpoint| checkpoint.id),
            source_entry_high_water,
            source_digest: digest.finish().ok_or(PersistenceError::InvalidState {
                reason: "a compaction source prefix is incomplete",
            })?,
            source,
            parent_summary,
            user_guidance: user_guidance
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(|text| bounded_excerpt(text, MAX_GUIDANCE_BYTES)),
            estimated_input_tokens,
        })
    }
}

/// Projection is only for lossy summaries, never tool replay or canonical storage.
fn project_entry(entry: &TranscriptEntry) -> Result<Option<String>, PersistenceError> {
    let text = match entry {
        TranscriptEntry::UserMessage {
            text, attachments, ..
        } => {
            let mut output = format!("USER:\n{text}");
            for image in attachments {
                output.push_str(&format!(
                    "\nIMAGE: {} · {} · {}x{}",
                    image.display_name,
                    image.media_type.as_str(),
                    image.width,
                    image.height
                ));
            }
            output
        }
        TranscriptEntry::AssistantMessage { text, .. } => format!("ASSISTANT:\n{text}"),
        TranscriptEntry::ToolCall { input, .. } => format!(
            "TOOL CALL {}:\n{}",
            input.kind().name(),
            input
                .provider_arguments()
                .map_err(|_| invalid_projection())?
        ),
        TranscriptEntry::ToolResult { result, .. } => format!(
            "TOOL RESULT:\n{}",
            result.provider_output().map_err(|_| invalid_projection())?
        ),
        TranscriptEntry::LocalCommand {
            context_visible: false,
            ..
        } => return Ok(None),
        TranscriptEntry::LocalCommand {
            command,
            status,
            stdout,
            stderr,
            ..
        } => format!("LOCAL COMMAND {status:?}:\n{command}\nstdout:\n{stdout}\nstderr:\n{stderr}"),
    };
    Ok(Some(format!("{text}\n\n")))
}

fn invalid_projection() -> PersistenceError {
    PersistenceError::InvalidState {
        reason: "a canonical tool entry cannot be projected",
    }
}

fn bounded_excerpt(text: &str, maximum: usize) -> String {
    const MARKER: &str = "\n[Source excerpt truncated; omitted text remains canonical.]\n";
    if text.len() <= maximum {
        return text.to_owned();
    }
    let keep = maximum.saturating_sub(MARKER.len());
    let mut head = keep / 2;
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - keep / 2;
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    format!("{}{MARKER}{}", &text[..head], &text[tail..])
}

#[cfg(test)]
#[path = "context_compaction/tests.rs"]
pub(super) mod tests;
