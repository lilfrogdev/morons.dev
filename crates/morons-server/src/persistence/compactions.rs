use sha2::{Digest, Sha256};

use super::TranscriptEntry;

const CONTEXT_SOURCE_DIGEST: &[u8] = b"morons.dev/context-source/v1\0";

/// Incremental implementation of the existing policy-v4 canonical prefix digest.
/// Excluded commands are hashed for integrity, never projected into model context.
pub(crate) struct ContextSourceHasher {
    digest: Sha256,
    high_water: u64,
    next_sequence: u64,
}

impl ContextSourceHasher {
    pub(crate) fn new(high_water: u64) -> Self {
        let mut digest = Sha256::new();
        digest.update(CONTEXT_SOURCE_DIGEST);
        digest.update(high_water.to_be_bytes());
        Self {
            digest,
            high_water,
            next_sequence: 1,
        }
    }

    pub(crate) fn push(&mut self, entry: &TranscriptEntry) -> Option<()> {
        if entry.entry_sequence() != self.next_sequence || self.next_sequence > self.high_water {
            return None;
        }
        self.next_sequence = self.next_sequence.checked_add(1)?;
        let digest = &mut self.digest;
        digest.update(entry.entry_sequence().to_be_bytes());
        match entry {
            TranscriptEntry::UserMessage {
                id,
                run_id,
                text,
                attachments,
                ..
            } => {
                digest.update([1]);
                digest.update(id.as_bytes());
                digest.update(run_id.as_bytes());
                update_text(digest, text)?;
                digest.update((attachments.len() as u32).to_be_bytes());
                for attachment in attachments {
                    digest.update(attachment.id.as_bytes());
                    update_text(digest, &attachment.display_name)?;
                    digest.update(attachment.marker_start.to_be_bytes());
                    digest.update([crate::persistence::images::media_type_record(
                        attachment.media_type,
                    )]);
                    digest.update(attachment.width.to_be_bytes());
                    digest.update(attachment.height.to_be_bytes());
                    digest.update(attachment.bytes.to_be_bytes());
                    digest.update(attachment.digest);
                }
            }
            TranscriptEntry::AssistantMessage {
                id,
                run_id,
                service,
                model_id,
                text,
                refusal,
                phase,
                ..
            } => {
                digest.update([2]);
                digest.update(id.as_bytes());
                digest.update(run_id.as_bytes());
                digest.update([match service {
                    super::RunOpenCodeService::Zen => 1,
                    super::RunOpenCodeService::Go => 2,
                }]);
                update_text(digest, model_id)?;
                update_text(digest, text)?;
                digest.update([u8::from(*refusal)]);
                digest.update([match phase {
                    super::AssistantMessagePhase::Commentary => 1,
                    super::AssistantMessagePhase::Final => 2,
                }]);
            }
            TranscriptEntry::ToolCall {
                id,
                run_id,
                call_id,
                operation_id,
                provider_operation_id,
                input,
                ..
            } => {
                digest.update([3]);
                digest.update(id.as_bytes());
                digest.update(run_id.as_bytes());
                digest.update(call_id.as_bytes());
                digest.update(operation_id.as_bytes());
                digest.update(provider_operation_id.as_bytes());
                update_bytes(digest, &serde_json::to_vec(input).ok()?)?;
            }
            TranscriptEntry::ToolResult {
                id,
                run_id,
                call_id,
                operation_id,
                tool,
                result,
                ..
            } => {
                digest.update([4]);
                digest.update(id.as_bytes());
                digest.update(run_id.as_bytes());
                digest.update(call_id.as_bytes());
                digest.update(operation_id.as_bytes());
                digest.update(tool.to_record().to_be_bytes());
                update_bytes(digest, &serde_json::to_vec(result).ok()?)?;
            }
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
            } => {
                digest.update([5]);
                digest.update(id.as_bytes());
                digest.update(command_id.as_bytes());
                update_text(digest, command)?;
                digest.update([u8::from(*context_visible)]);
                digest.update([match status {
                    super::LocalCommandStatus::Succeeded => 1,
                    super::LocalCommandStatus::Failed => 2,
                    super::LocalCommandStatus::Interrupted => 3,
                    super::LocalCommandStatus::Uncertain => 4,
                }]);
                digest.update(exit_code.unwrap_or(i32::MIN).to_be_bytes());
                digest.update(signal.unwrap_or(u16::MAX).to_be_bytes());
                update_text(digest, stdout)?;
                update_text(digest, stderr)?;
            }
        }
        Some(())
    }

    pub(crate) fn finish(self) -> Option<[u8; 32]> {
        (self.high_water > 0 && self.next_sequence == self.high_water.checked_add(1)?)
            .then(|| self.digest.finalize().into())
    }
}

fn update_text(digest: &mut Sha256, value: &str) -> Option<()> {
    update_bytes(digest, value.as_bytes())
}

fn update_bytes(digest: &mut Sha256, value: &[u8]) -> Option<()> {
    digest.update(u64::try_from(value.len()).ok()?.to_be_bytes());
    digest.update(value);
    Some(())
}

#[cfg(test)]
#[path = "compactions/tests.rs"]
mod tests;
