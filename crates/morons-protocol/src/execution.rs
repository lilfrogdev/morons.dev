use serde::{Deserialize, Serialize};

pub const MAX_EXECUTION_IMAGE_SOURCE_PATH_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionImageState {
    Unconfigured,
    Provisioning,
    Ready,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionTargetOs {
    Macos,
    Linux,
    Windows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionTargetArch {
    X86_64,
    Aarch64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionImageSummary {
    pub state: ExecutionImageState,
    pub target_os: ExecutionTargetOs,
    pub target_arch: ExecutionTargetArch,
    pub format_version: u16,
    pub limits_version: u16,
    pub file_count: u64,
    pub logical_bytes: u64,
}
