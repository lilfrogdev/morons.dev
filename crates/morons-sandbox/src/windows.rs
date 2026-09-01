mod direct;
mod staging;

use std::{
    io::{Seek, SeekFrom},
    thread,
    time::{Duration, Instant},
};

use rappct::{
    JobLimits, LaunchOptions, StdioConfig,
    launch::{LaunchedIo, launch_in_container_with_io},
};

use crate::{
    Cancellation, SandboxRequest, SandboxResult, SandboxStatus, read_result,
    runner::PreparedRequest,
};

use staging::{Container, Layout, bootstrap_environment, command_line, launch_path};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const TREE_TERMINATION_TIMEOUT: Duration = Duration::from_secs(2);
const PROCESS_MEMORY_BYTES: usize = 2 * 1024 * 1024 * 1024;

pub(crate) fn execute(
    prepared: PreparedRequest,
    request: SandboxRequest,
    cancellation: &Cancellation,
) -> SandboxResult {
    let operation_id = prepared.operation_id;
    let layout = match Layout::prepare(&prepared) {
        Ok(layout) => layout,
        Err(()) => {
            diagnostic("staging");
            return SandboxResult::failure(operation_id, SandboxStatus::LaunchFailed);
        }
    };
    let stage_request = match layout.stage_request(&prepared, &request) {
        Ok(request) => request,
        Err(()) => {
            diagnostic("stage-request");
            let _ = layout.cleanup();
            return SandboxResult::failure(operation_id, SandboxStatus::RequestRejected);
        }
    };
    let container = match Container::create(operation_id) {
        Ok(container) => container,
        Err(()) => {
            diagnostic("profile");
            let _ = layout.cleanup();
            return SandboxResult::failure(operation_id, SandboxStatus::BackendUnavailable);
        }
    };
    if container.grant_paths(&prepared, &layout).is_err() {
        diagnostic("grants");
        return cleanup_failure(
            operation_id,
            SandboxStatus::BackendUnavailable,
            container,
            &layout,
        );
    }
    let mut output = match layout.initialize_control(&stage_request) {
        Ok(output) => output,
        Err(()) => {
            diagnostic("control");
            return cleanup_failure(
                operation_id,
                SandboxStatus::BackendUnavailable,
                container,
                &layout,
            );
        }
    };
    let capabilities = match container.capabilities() {
        Ok(capabilities) => capabilities,
        Err(()) => {
            diagnostic("capabilities");
            drop(output);
            return cleanup_failure(
                operation_id,
                SandboxStatus::BackendUnavailable,
                container,
                &layout,
            );
        }
    };
    let options = match launch_options(&prepared, &layout) {
        Ok(options) => options,
        Err(()) => {
            diagnostic("launch-options");
            drop(output);
            return cleanup_failure(
                operation_id,
                SandboxStatus::BackendUnavailable,
                container,
                &layout,
            );
        }
    };
    let launched = match launch_in_container_with_io(&capabilities, &options) {
        Ok(launched) => launched,
        Err(error) => {
            diagnostic_launch(&error);
            drop(output);
            return cleanup_failure(
                operation_id,
                SandboxStatus::BackendUnavailable,
                container,
                &layout,
            );
        }
    };
    if launched.job_guard.is_none() {
        diagnostic("job");
        let stopped = stop_without_job(launched);
        drop(output);
        return if stopped {
            cleanup_failure(
                operation_id,
                SandboxStatus::BackendUnavailable,
                container,
                &layout,
            )
        } else {
            SandboxResult::failure(operation_id, SandboxStatus::ProcessTreeUncertain)
        };
    }
    if layout.open_gate().is_err() {
        diagnostic("gate");
        drop(output);
        return stop_and_cleanup(
            operation_id,
            SandboxStatus::BackendUnavailable,
            launched,
            container,
            &layout,
        );
    }

    let deadline = Instant::now()
        + Duration::from_millis(prepared.wall_time_milliseconds)
        + TREE_TERMINATION_TIMEOUT;
    loop {
        if cancellation.is_cancelled() {
            drop(output);
            return stop_and_cleanup(
                operation_id,
                SandboxStatus::Cancelled,
                launched,
                container,
                &layout,
            );
        }
        if Instant::now() >= deadline {
            drop(output);
            return stop_and_cleanup(
                operation_id,
                SandboxStatus::TimedOut,
                launched,
                container,
                &layout,
            );
        }
        if layout.done.is_file() {
            if output.seek(SeekFrom::Start(0)).is_err() {
                diagnostic("result-seek");
                drop(output);
                return stop_and_cleanup(
                    operation_id,
                    SandboxStatus::BackendUnavailable,
                    launched,
                    container,
                    &layout,
                );
            }
            let result = read_result(&mut output);
            let stopped = stop_job(launched);
            drop(output);
            if !stopped {
                return SandboxResult::failure(operation_id, SandboxStatus::ProcessTreeUncertain);
            }
            let cleaned = delete_and_cleanup(container, &layout);
            return match result {
                Ok(result) if cleaned && result.operation_id == operation_id => result,
                Ok(_) | Err(_) => {
                    diagnostic("nested-result");
                    SandboxResult::failure(operation_id, SandboxStatus::BackendUnavailable)
                }
            };
        }
        thread::sleep(POLL_INTERVAL);
    }
}

pub fn run_file_stage(
    input: &std::path::Path,
    output: &std::path::Path,
    gate: &std::path::Path,
    done: &std::path::Path,
) -> std::process::ExitCode {
    direct::run_file_stage(input, output, gate, done)
}

fn launch_options(prepared: &PreparedRequest, layout: &Layout) -> Result<LaunchOptions, ()> {
    let executable = launch_path(&layout.runner)?;
    Ok(LaunchOptions {
        exe: executable.clone(),
        cmdline: Some(command_line(&executable, layout)?),
        cwd: Some(launch_path(&prepared.candidate_root)?),
        env: Some(bootstrap_environment(layout)?),
        stdio: StdioConfig::Null,
        suspended: false,
        join_job: Some(JobLimits {
            memory_bytes: Some(PROCESS_MEMORY_BYTES),
            cpu_rate_percent: None,
            kill_on_job_close: true,
        }),
        startup_timeout: None,
    })
}

fn stop_and_cleanup(
    operation_id: [u8; 16],
    status: SandboxStatus,
    launched: LaunchedIo,
    container: Container,
    layout: &Layout,
) -> SandboxResult {
    if !stop_job(launched) {
        return SandboxResult::failure(operation_id, SandboxStatus::ProcessTreeUncertain);
    }
    cleanup_failure(operation_id, status, container, layout)
}

fn stop_job(mut launched: LaunchedIo) -> bool {
    let Some(job) = launched.job_guard.take() else {
        return false;
    };
    drop(job);
    launched.wait(Some(TREE_TERMINATION_TIMEOUT)).is_ok()
}

fn stop_without_job(launched: LaunchedIo) -> bool {
    launched.wait(Some(TREE_TERMINATION_TIMEOUT)).is_ok()
}

fn cleanup_failure(
    operation_id: [u8; 16],
    status: SandboxStatus,
    container: Container,
    layout: &Layout,
) -> SandboxResult {
    let cleaned = delete_and_cleanup(container, layout);
    SandboxResult::failure(
        operation_id,
        if cleaned {
            status
        } else {
            SandboxStatus::BackendUnavailable
        },
    )
}

fn delete_and_cleanup(container: Container, layout: &Layout) -> bool {
    let profile_deleted = container.delete().is_ok();
    let staging_removed = layout.cleanup().is_ok();
    profile_deleted && staging_removed
}

fn diagnostic_launch(error: &rappct::AcError) {
    let stage = match error {
        rappct::AcError::LaunchFailed { stage, .. } => stage,
        rappct::AcError::AccessDenied { .. } => "launch-access-denied",
        rappct::AcError::Win32(_) => "launch-win32",
        rappct::AcError::UnsupportedPlatform => "launch-unsupported-platform",
        rappct::AcError::UnsupportedLpac => "launch-unsupported-lpac",
        rappct::AcError::UnknownCapability { .. } => "launch-capability",
        rappct::AcError::Unimplemented(_) => "launch-unimplemented",
    };
    diagnostic(stage);
}

fn diagnostic(stage: &'static str) {
    if std::env::var_os("MORONS_SANDBOX_TEST_DIAGNOSTICS").is_some() {
        eprintln!("windows sandbox stage: {stage}");
    }
}
