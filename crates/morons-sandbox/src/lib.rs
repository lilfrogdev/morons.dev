mod protocol;
mod runner;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
#[doc(hidden)]
pub use linux::{run_command_stage, run_namespace_stage, run_pid_stage};
#[cfg(target_os = "windows")]
#[doc(hidden)]
pub use windows::run_file_stage as run_windows_file_stage;

pub use protocol::{
    SANDBOX_PROTOCOL_VERSION, SandboxExit, SandboxLimits, SandboxRequest, SandboxResult,
    SandboxStatus, read_request, read_result, write_request, write_result,
};
pub use runner::{Cancellation, execute};
