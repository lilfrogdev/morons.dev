use super::*;
use crate::terminal::TranscriptText;

pub(crate) struct SessionView {
    pub(crate) summary: SessionSummary,
    pub(crate) display_name: SafeText,
    pub(crate) entries: Vec<PresentedTranscriptEntry>,
    pub(super) source_bytes: usize,
    delta_sequence: Option<(RunId, u64)>,
    pub(crate) runs: Vec<RunSummary>,
    pub(crate) active_run_id: Option<RunId>,
    pub(crate) active_command_id: Option<LocalCommandId>,
    pub(crate) older_cursor: Option<TranscriptCursor>,
    pub(crate) newer_cursor: Option<TranscriptCursor>,
    pub(super) deferred_newer_output: bool,
    pub(super) tail_refresh_required: bool,
    pub(crate) skills: Vec<PresentedSkill>,
    pub(crate) transient: Option<TransientAssistant>,
    pub(crate) context_status: Option<SessionContextStatus>,
    pub(crate) shared_directory: bool,
}

impl SessionView {
    pub(super) fn new(
        window: TranscriptWindowData,
        skills: Vec<SkillSummary>,
    ) -> Result<Self, UiStateError> {
        let TranscriptWindowData {
            summary,
            entries,
            runs,
            active_run_id,
            active_command_id,
            older_cursor,
            newer_cursor,
        } = window;
        if entries.len() > MAX_CLIENT_TRANSCRIPT_ENTRIES || runs.len() > MAX_CLIENT_RUNS {
            return Err(UiStateError::ResourceLimitExceeded);
        }
        if runs.iter().any(|run| run.session_id != summary.id)
            || [older_cursor.as_ref(), newer_cursor.as_ref()]
                .iter()
                .flatten()
                .any(|cursor| cursor.as_bytes()[..16] != summary.id.as_bytes()[..])
            || entries.iter().any(|entry| {
                transcript_entry_run_id(entry)
                    .is_some_and(|run_id| !runs.iter().any(|run| run.id == run_id))
            })
            || active_run_id.is_some_and(|active_run_id| {
                !runs
                    .iter()
                    .any(|run| run.id == active_run_id && !run.state.is_terminal())
            })
            || (active_run_id.is_some() && active_command_id.is_some())
        {
            return Err(UiStateError::ResourceScopeMismatch);
        }
        let source_bytes = crate::transcript_budget::window_source_bytes(&entries)
            .ok_or(UiStateError::ResourceLimitExceeded)?;
        let display_name = SafeText::from_untrusted(
            summary
                .display_name
                .as_deref()
                .unwrap_or("Untitled session"),
        );
        Ok(Self {
            summary,
            display_name,
            entries: entries
                .into_iter()
                .map(PresentedTranscriptEntry::new)
                .collect::<Result<_, _>>()?,
            source_bytes,
            delta_sequence: None,
            runs,
            active_run_id,
            active_command_id,
            older_cursor,
            newer_cursor,
            deferred_newer_output: false,
            tail_refresh_required: false,
            skills: skills.into_iter().map(PresentedSkill::new).collect(),
            transient: None,
            context_status: None,
            shared_directory: false,
        })
    }

    pub(super) fn is_historical_window(&self) -> bool {
        self.newer_cursor.is_some() || self.deferred_newer_output
    }

    pub(super) fn defer_transcript_entry(&mut self, entry: &TranscriptEntry) {
        if let TranscriptEntry::LocalCommand { command_id, .. } = entry
            && self.active_command_id == Some(*command_id)
        {
            self.active_command_id = None;
        }
        if let TranscriptEntry::AssistantMessage { run_id, .. } = entry
            && self
                .transient
                .as_ref()
                .is_some_and(|transient| transient.run_id == *run_id)
        {
            self.transient = None;
        }
        self.deferred_newer_output = true;
    }

    pub(super) fn append_transcript_entry(
        &mut self,
        entry: TranscriptEntry,
    ) -> Result<bool, UiStateError> {
        let bytes = crate::transcript_budget::entry_source_bytes(&entry)
            .ok_or(UiStateError::ResourceLimitExceeded)?;
        let id = transcript_entry_id(&entry);
        if self.entries.iter().any(|existing| existing.id == id) {
            return Ok(false);
        }
        let next_bytes = self
            .source_bytes
            .checked_add(bytes)
            .ok_or(UiStateError::ResourceLimitExceeded)?;
        if self.entries.len() >= MAX_CLIENT_TRANSCRIPT_ENTRIES
            || next_bytes > crate::transcript_budget::MAX_SOURCE_BYTES
        {
            self.defer_transcript_entry(&entry);
            self.tail_refresh_required = true;
            return Ok(true);
        }
        let run_id = transcript_entry_run_id(&entry);
        if matches!(entry, TranscriptEntry::AssistantMessage { .. })
            && self
                .transient
                .as_ref()
                .is_some_and(|transient| Some(transient.run_id) == run_id)
        {
            self.transient = None;
        }
        if let TranscriptEntry::LocalCommand { command_id, .. } = &entry
            && self.active_command_id == Some(*command_id)
        {
            self.active_command_id = None;
        }
        self.entries.push(PresentedTranscriptEntry::new(entry)?);
        self.source_bytes = next_bytes;
        Ok(true)
    }

    pub(super) fn apply_run(&mut self, run: RunSummary) -> Result<(), UiStateError> {
        if run.session_id != self.summary.id {
            return Err(UiStateError::ResourceScopeMismatch);
        }
        if let Some(existing) = self.runs.iter_mut().find(|existing| existing.id == run.id) {
            if !valid_run_transition(existing.state, run.state) {
                return Err(UiStateError::InvalidRunTransition);
            }
            *existing = run.clone();
        } else {
            if self.runs.len() >= MAX_CLIENT_RUNS {
                return Err(UiStateError::ResourceLimitExceeded);
            }
            self.runs.push(run.clone());
        }
        if run.state.is_terminal() {
            if self.delta_sequence.is_some_and(|(id, _)| id == run.id) {
                self.delta_sequence = None;
            }
            if self.active_run_id == Some(run.id) {
                self.active_run_id = None;
            }
            if self
                .transient
                .as_ref()
                .is_some_and(|transient| transient.run_id == run.id)
            {
                self.transient = None;
            }
            if self.is_historical_window()
                && !self
                    .entries
                    .iter()
                    .any(|entry| entry.run_id == Some(run.id))
            {
                self.runs.retain(|existing| existing.id != run.id);
            }
        } else {
            if self
                .active_run_id
                .is_some_and(|active_run| active_run != run.id)
            {
                return Err(UiStateError::InvalidRunTransition);
            }
            self.active_run_id = Some(run.id);
        }
        Ok(())
    }

    pub(super) fn install_context_status(
        &mut self,
        context: SessionContextStatus,
    ) -> Result<(), UiStateError> {
        if context.session_id != self.summary.id {
            return Err(UiStateError::ResourceScopeMismatch);
        }
        self.context_status = Some(context);
        Ok(())
    }

    pub(super) fn pause_preview(&mut self) {
        self.delta_sequence = None;
        if let Some(transient) = &mut self.transient {
            transient.truncated = true;
        }
    }

    pub(super) fn append_delta(
        &mut self,
        run_id: RunId,
        sequence: u64,
        delta: &str,
        refusal: bool,
    ) -> Result<(), UiStateError> {
        if self.active_run_id != Some(run_id) {
            return Err(UiStateError::InvalidRunTransition);
        }
        let previous = self
            .delta_sequence
            .filter(|(id, _)| *id == run_id)
            .map_or(0, |(_, sequence)| sequence);
        if sequence <= previous {
            return Err(UiStateError::InvalidRunTransition);
        }
        let contiguous = previous.checked_add(1) == Some(sequence);
        self.delta_sequence = Some((run_id, sequence));
        let transient = self
            .transient
            .get_or_insert_with(|| TransientAssistant::new(run_id, refusal));
        if transient.run_id != run_id || transient.refusal != refusal {
            return Err(UiStateError::InvalidRunTransition);
        }
        transient.truncated |= !contiguous;
        if transient.truncated {
            return Ok(());
        }
        let Some(next_length) = transient.text.len().checked_add(delta.len()) else {
            transient.truncated = true;
            return Ok(());
        };
        if next_length > MAX_TRANSIENT_DELTA_BYTES {
            transient.truncated = true;
            return Ok(());
        }
        transient.text.push_str(delta);
        transient.presented = transcript_text(&transient.text)?;
        Ok(())
    }
}

pub(crate) struct PresentedTranscriptEntry {
    pub(crate) id: MessageId,
    pub(crate) run_id: Option<RunId>,
    pub(super) command_id: Option<LocalCommandId>,
    pub(crate) role: &'static str,
    pub(crate) text: TranscriptText,
    pub(crate) refusal: bool,
}

impl PresentedTranscriptEntry {
    pub(super) fn new(entry: TranscriptEntry) -> Result<Self, UiStateError> {
        crate::transcript_budget::entry_source_bytes(&entry)
            .ok_or(UiStateError::ResourceLimitExceeded)?;
        Ok(match entry {
            TranscriptEntry::UserMessage {
                id, run_id, text, ..
            } => Self {
                id,
                run_id: Some(run_id),
                command_id: None,
                role: "You",
                text: transcript_text(&text)?,
                refusal: false,
            },
            TranscriptEntry::AssistantMessage {
                id,
                run_id,
                text,
                refusal,
                ..
            } => Self {
                id,
                run_id: Some(run_id),
                command_id: None,
                role: "Assistant",
                text: transcript_text(&text)?,
                refusal,
            },
            TranscriptEntry::ToolCall {
                id,
                run_id,
                tool,
                path,
                ..
            } => Self {
                id,
                run_id: Some(run_id),
                command_id: None,
                role: "Tool call",
                text: transcript_text(&format!("{} · {path}", tool_label(tool)))?,
                refusal: false,
            },
            TranscriptEntry::ToolResult {
                id,
                run_id,
                tool,
                status,
                summary,
                ..
            } => Self {
                id,
                run_id: Some(run_id),
                command_id: None,
                role: "Tool result",
                text: transcript_text(&format!("{} · {status:?} · {summary}", tool_label(tool)))?,
                refusal: false,
            },
            TranscriptEntry::LocalCommand {
                id,
                command_id,
                command,
                context_visible,
                status,
                exit_code,
                signal,
                stdout,
                stderr,
                ..
            } => Self {
                id,
                run_id: None,
                command_id: Some(command_id),
                role: if context_visible {
                    "Command !"
                } else {
                    "Command !!"
                },
                text: transcript_text(&format!(
                    "{status:?} · exit {exit_code:?} · signal {signal:?}\n$ {command}\nstdout:\n{stdout}\nstderr:\n{stderr}"
                ))?,
                refusal: false,
            },
        })
    }
}

fn transcript_text(text: &str) -> Result<TranscriptText, UiStateError> {
    TranscriptText::from_untrusted(text).map_err(|_| UiStateError::ResourceLimitExceeded)
}

pub(crate) struct TransientAssistant {
    pub(crate) run_id: RunId,
    pub(super) text: String,
    pub(crate) presented: TranscriptText,
    pub(crate) refusal: bool,
    pub(crate) truncated: bool,
}

impl TransientAssistant {
    pub(super) fn new(run_id: RunId, refusal: bool) -> Self {
        Self {
            run_id,
            text: String::new(),
            presented: TranscriptText::from_untrusted("").expect("empty transcript fits"),
            refusal,
            truncated: false,
        }
    }
}

impl fmt::Debug for TransientAssistant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransientAssistant")
            .field("run_id", &self.run_id)
            .field("text_bytes", &self.text.len())
            .field("refusal", &self.refusal)
            .field("truncated", &self.truncated)
            .finish()
    }
}
