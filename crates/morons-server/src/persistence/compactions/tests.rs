use super::ContextSourceHasher;
use crate::persistence::{LocalCommandId, LocalCommandStatus, MessageId, RunId, TranscriptEntry};

fn entries() -> [TranscriptEntry; 2] {
    [
        TranscriptEntry::UserMessage {
            entry_sequence: 1,
            id: MessageId::from_bytes([1; 16]),
            run_id: RunId::from_bytes([2; 16]),
            text: "hello".to_owned(),
            attachments: Vec::new(),
            created_at_milliseconds: 0,
        },
        TranscriptEntry::LocalCommand {
            entry_sequence: 2,
            id: MessageId::from_bytes([3; 16]),
            command_id: LocalCommandId::from_bytes([4; 16]),
            command: "hidden".to_owned(),
            context_visible: false,
            status: LocalCommandStatus::Succeeded,
            exit_code: Some(0),
            signal: None,
            stdout: "hidden output".to_owned(),
            stderr: String::new(),
            created_at_milliseconds: 0,
        },
    ]
}

#[test]
fn streaming_digest_preserves_v4_prefix_format_and_hidden_entry_binding() {
    let mut digest = ContextSourceHasher::new(2);
    for entry in entries() {
        digest.push(&entry).unwrap();
    }
    let bytes = digest.finish().unwrap();
    let hex = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    // Independent SHA-256 vector for the pre-hardening policy-v4 byte format.
    assert_eq!(
        hex,
        "ae554d739b6e62e11eb69522d3ebe09b429cfd3b615889851028aae961272141"
    );
    let mut changed = entries();
    if let TranscriptEntry::LocalCommand { stdout, .. } = &mut changed[1] {
        stdout.push('!');
    }
    let mut digest = ContextSourceHasher::new(2);
    for entry in changed {
        digest.push(&entry).unwrap();
    }
    assert_ne!(digest.finish(), Some(bytes));
}

#[test]
fn streaming_digest_rejects_gaps_duplicates_and_incomplete_coverage() {
    let entries = entries();
    assert!(ContextSourceHasher::new(0).finish().is_none());
    assert!(ContextSourceHasher::new(2).push(&entries[1]).is_none());
    let mut digest = ContextSourceHasher::new(2);
    digest.push(&entries[0]).unwrap();
    assert!(digest.finish().is_none());
    let mut digest = ContextSourceHasher::new(2);
    digest.push(&entries[0]).unwrap();
    assert!(digest.push(&entries[0]).is_none());
}
