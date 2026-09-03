use sha2::{Digest, Sha256};

use super::TranscriptEntry;

const CONTEXT_SOURCE_DIGEST: &[u8] = b"morons.dev/context-source/v1\0";

pub(crate) fn context_source_digest(
    entries: &[TranscriptEntry],
    high_water: u64,
) -> Option<[u8; 32]> {
    let covered = entries
        .iter()
        .take_while(|entry| entry.entry_sequence() <= high_water)
        .collect::<Vec<_>>();
    if covered.is_empty()
        || covered.last().map(|entry| entry.entry_sequence()) != Some(high_water)
        || covered
            .iter()
            .enumerate()
            .any(|(index, entry)| entry.entry_sequence() != index as u64 + 1)
    {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(CONTEXT_SOURCE_DIGEST);
    digest.update(high_water.to_be_bytes());
    for entry in covered {
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
                update_text(&mut digest, text)?;
                digest.update((attachments.len() as u32).to_be_bytes());
                for attachment in attachments {
                    digest.update(attachment.id.as_bytes());
                    update_text(&mut digest, &attachment.display_name)?;
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
                update_text(&mut digest, model_id)?;
                update_text(&mut digest, text)?;
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
                let payload = serde_json::to_vec(input).ok()?;
                update_bytes(&mut digest, &payload)?;
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
                let payload = serde_json::to_vec(result).ok()?;
                update_bytes(&mut digest, &payload)?;
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
                update_text(&mut digest, command)?;
                digest.update([u8::from(*context_visible)]);
                digest.update([match status {
                    super::LocalCommandStatus::Succeeded => 1,
                    super::LocalCommandStatus::Failed => 2,
                    super::LocalCommandStatus::Interrupted => 3,
                    super::LocalCommandStatus::Uncertain => 4,
                }]);
                digest.update(exit_code.unwrap_or(i32::MIN).to_be_bytes());
                digest.update(signal.unwrap_or(u16::MAX).to_be_bytes());
                update_text(&mut digest, stdout)?;
                update_text(&mut digest, stderr)?;
            }
        }
    }
    Some(digest.finalize().into())
}

fn update_text(digest: &mut Sha256, value: &str) -> Option<()> {
    update_bytes(digest, value.as_bytes())
}

fn update_bytes(digest: &mut Sha256, value: &[u8]) -> Option<()> {
    digest.update(u64::try_from(value.len()).ok()?.to_be_bytes());
    digest.update(value);
    Some(())
}
