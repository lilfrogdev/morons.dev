use morons_protocol::TranscriptEntry;

pub(crate) const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const ENTRY_ENVELOPE_BYTES: usize = 128;

pub(crate) fn entry_id(entry: &TranscriptEntry) -> morons_protocol::MessageId {
    match entry {
        TranscriptEntry::UserMessage { id, .. }
        | TranscriptEntry::AssistantMessage { id, .. }
        | TranscriptEntry::ToolCall { id, .. }
        | TranscriptEntry::ToolResult { id, .. }
        | TranscriptEntry::LocalCommand { id, .. } => *id,
    }
}

pub(crate) fn entry_source_bytes(entry: &TranscriptEntry) -> Option<usize> {
    let bytes = match entry {
        TranscriptEntry::UserMessage { text, .. }
        | TranscriptEntry::AssistantMessage { text, .. } => text.len(),
        TranscriptEntry::ToolCall { path, .. } => path.len().checked_add(ENTRY_ENVELOPE_BYTES)?,
        TranscriptEntry::ToolResult { summary, .. } => {
            summary.len().checked_add(ENTRY_ENVELOPE_BYTES)?
        }
        TranscriptEntry::LocalCommand {
            command,
            stdout,
            stderr,
            ..
        } => command
            .len()
            .checked_add(stdout.len())?
            .checked_add(stderr.len())?
            .checked_add(ENTRY_ENVELOPE_BYTES)?,
    };
    (bytes <= MAX_SOURCE_BYTES).then_some(bytes)
}

pub(crate) fn window_source_bytes(entries: &[TranscriptEntry]) -> Option<usize> {
    entries.iter().try_fold(0_usize, |total, entry| {
        total
            .checked_add(entry_source_bytes(entry)?)
            .filter(|bytes| *bytes <= MAX_SOURCE_BYTES)
    })
}
