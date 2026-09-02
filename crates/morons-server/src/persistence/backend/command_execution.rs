use serde::{Deserialize, Serialize};

pub(super) const COMMAND_POLICY_VERSION: u16 = 1;
pub(super) const COMMAND_LIMITS_VERSION: u16 = 1;

/// Durable shape retained only to validate and clean up historical sandbox-command operations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommandBinding {
    pub workspace_id: [u8; 16],
    pub active_generation_id: [u8; 16],
    pub generation_id: [u8; 16],
    pub image_generation_id: [u8; 16],
    pub image_manifest_digest: [u8; 32],
    pub source_manifest_digest: [u8; 32],
    pub source_file_count: u64,
    pub source_directory_count: u64,
    pub source_logical_bytes: u64,
    pub policy_version: u16,
    pub limits_version: u16,
}

pub(crate) fn command_binding_is_valid(payload: &[u8]) -> bool {
    serde_json::from_slice::<CommandBinding>(payload).is_ok_and(|binding| {
        binding.workspace_id.iter().any(|byte| *byte != 0)
            && binding.active_generation_id.iter().any(|byte| *byte != 0)
            && binding.generation_id.iter().any(|byte| *byte != 0)
            && binding.image_generation_id.iter().any(|byte| *byte != 0)
            && binding.policy_version == COMMAND_POLICY_VERSION
            && binding.limits_version == COMMAND_LIMITS_VERSION
            && binding.generation_id != binding.active_generation_id
    })
}
