use serde::{Deserialize, Serialize};

use super::{MAX_FILE_BYTES, ToolErrorKind, WorktreePath};

const RECOVERY_PLAN_VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryPlan {
    version: u16,
    kind: MutationKind,
    path: WorktreePath,
    temporary_name: String,
    parent: NodeSnapshot,
    before: Option<NodeSnapshot>,
    staged: NodeSnapshot,
    before_sha256: Option<String>,
    after_sha256: Option<String>,
    after_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MutationKind {
    EditFile,
    CreateFile,
    CreateDirectory,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "snake_case", deny_unknown_fields)]
enum NodeSnapshot {
    Unix {
        device: u64,
        inode: u64,
        mode: u32,
        links: u64,
        size: u64,
        modified_seconds: i64,
        modified_nanoseconds: i64,
        changed_seconds: i64,
        changed_nanoseconds: i64,
    },
    Windows {
        volume_serial: u64,
        file_id: String,
        kind: SnapshotNodeKind,
        size: u64,
        allocation_size: u64,
        link_count: u32,
        attributes: u32,
        reparse_tag: Option<u32>,
        creation_time: i64,
        last_write_time: i64,
        change_time: i64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SnapshotNodeKind {
    File,
    Directory,
    ReparsePoint,
}

pub(crate) fn recovery_plan_is_valid(bytes: &[u8]) -> bool {
    decode_plan(bytes).is_ok()
}

fn decode_plan(bytes: &[u8]) -> Result<RecoveryPlan, ToolErrorKind> {
    if bytes.len() > super::MAX_TOOL_PAYLOAD_BYTES {
        return Err(ToolErrorKind::ResourceLimit);
    }
    let plan: RecoveryPlan =
        serde_json::from_slice(bytes).map_err(|_| ToolErrorKind::Filesystem)?;
    if plan.version != RECOVERY_PLAN_VERSION
        || plan.temporary_name.len() != ".morons-tool-".len() + 32
        || !plan.temporary_name.starts_with(".morons-tool-")
        || !plan.temporary_name[".morons-tool-".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || plan.after_bytes > MAX_FILE_BYTES
        || plan
            .before_sha256
            .as_deref()
            .is_some_and(|value| !valid_sha256(value))
        || plan
            .after_sha256
            .as_deref()
            .is_some_and(|value| !valid_sha256(value))
        || !valid_recovery_plan(&plan)
    {
        return Err(ToolErrorKind::Filesystem);
    }
    Ok(plan)
}

fn valid_recovery_plan(plan: &RecoveryPlan) -> bool {
    if plan.path.parent_and_name().is_err()
        || !valid_node_snapshot(&plan.parent, SnapshotNodeKind::Directory)
    {
        return false;
    }
    match plan.kind {
        MutationKind::EditFile => {
            plan.before
                .as_ref()
                .is_some_and(|before| valid_node_snapshot(before, SnapshotNodeKind::File))
                && valid_node_snapshot(&plan.staged, SnapshotNodeKind::File)
                && plan.before_sha256.is_some()
                && plan.after_sha256.is_some()
        }
        MutationKind::CreateFile => {
            plan.before.is_none()
                && valid_node_snapshot(&plan.staged, SnapshotNodeKind::File)
                && plan.before_sha256.is_none()
                && plan.after_sha256.is_some()
        }
        MutationKind::CreateDirectory => {
            plan.before.is_none()
                && valid_node_snapshot(&plan.staged, SnapshotNodeKind::Directory)
                && plan.before_sha256.is_none()
                && plan.after_sha256.is_none()
                && plan.after_bytes == 0
        }
    }
}

fn valid_node_snapshot(snapshot: &NodeSnapshot, expected: SnapshotNodeKind) -> bool {
    match snapshot {
        NodeSnapshot::Unix {
            mode,
            links,
            size,
            modified_nanoseconds,
            changed_nanoseconds,
            ..
        } => {
            let kind = match mode & 0o170_000 {
                0o100_000 => SnapshotNodeKind::File,
                0o040_000 => SnapshotNodeKind::Directory,
                _ => return false,
            };
            kind == expected
                && *links > 0
                && (expected == SnapshotNodeKind::Directory || *size <= MAX_FILE_BYTES)
                && (0..1_000_000_000).contains(modified_nanoseconds)
                && (0..1_000_000_000).contains(changed_nanoseconds)
        }
        NodeSnapshot::Windows {
            file_id,
            kind,
            size,
            reparse_tag,
            ..
        } => {
            *kind == expected
                && *kind != SnapshotNodeKind::ReparsePoint
                && reparse_tag.is_none()
                && (expected == SnapshotNodeKind::Directory || *size <= MAX_FILE_BYTES)
                && file_id.len() == 32
                && file_id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historical_recovery_plan_decoder_remains_strict() {
        let plan = serde_json::json!({
            "version": 1,
            "kind": "create_file",
            "path": "file.txt",
            "temporary_name": ".morons-tool-11111111111111111111111111111111",
            "parent": {
                "platform": "unix",
                "device": 1,
                "inode": 2,
                "mode": 16832,
                "links": 1,
                "size": 0,
                "modified_seconds": 1,
                "modified_nanoseconds": 0,
                "changed_seconds": 1,
                "changed_nanoseconds": 0
            },
            "before": null,
            "staged": {
                "platform": "unix",
                "device": 1,
                "inode": 3,
                "mode": 33152,
                "links": 1,
                "size": 4,
                "modified_seconds": 1,
                "modified_nanoseconds": 0,
                "changed_seconds": 1,
                "changed_nanoseconds": 0
            },
            "before_sha256": null,
            "after_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "after_bytes": 4
        });
        let encoded = serde_json::to_vec(&plan).unwrap();
        assert!(recovery_plan_is_valid(&encoded));
        let mut invalid = plan;
        invalid["temporary_name"] = serde_json::Value::String("../escape".to_owned());
        assert!(!recovery_plan_is_valid(
            &serde_json::to_vec(&invalid).unwrap()
        ));
    }
}
