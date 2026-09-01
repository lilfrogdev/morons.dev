mod landlock;
mod platform;
mod seccomp;
mod supervision;

use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::OpenOptionsExt,
        process::{CommandExt, ExitStatusExt},
    },
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use rustix::{
    process::{Pid, Signal, set_parent_process_death_signal},
    thread::{no_new_privs, set_no_new_privs},
};

use crate::{
    Cancellation, SANDBOX_PROTOCOL_VERSION, SandboxExit, SandboxRequest, SandboxResult,
    SandboxStatus,
    runner::{PreparedRequest, validate_request},
    write_request,
};

use platform::{
    Layout, apply_limits, bind_to_parent, create_namespaces, drop_capabilities, mount_proc,
    namespace_identity, prepared_command_matches, setup_mounts, trusted_current_executable,
    validate_synthetic_target, verify_capabilities_dropped, write_failed_marker,
};
use supervision::{
    MAX_SANDBOX_TASKS, POLL_INTERVAL, READY_BYTES, RESOURCE_LIMIT_BYTES, capture_stream,
    child_exited_without_reaping, join_stream, namespace_task_count, read_stage_outcome,
    stop_outer_setup_failure, terminate_inner_group, terminate_outer_group, write_stage_marker,
    write_stage_outcome,
};

const STAGE_FAILURE: u8 = 125;

pub(crate) fn execute(
    prepared: PreparedRequest,
    request: SandboxRequest,
    cancellation: &Cancellation,
) -> SandboxResult {
    if !prepared_command_matches(&prepared, &request) {
        return SandboxResult::failure(prepared.operation_id, SandboxStatus::RequestRejected);
    }
    let layout = match Layout::prepare(&prepared) {
        Ok(layout) => layout,
        Err(()) => {
            return SandboxResult::failure(prepared.operation_id, SandboxStatus::LaunchFailed);
        }
    };
    let executable = match trusted_current_executable() {
        Ok(executable) => executable,
        Err(()) => {
            let _ = layout.cleanup();
            return SandboxResult::failure(
                prepared.operation_id,
                SandboxStatus::BackendUnavailable,
            );
        }
    };

    let mut command = Command::new(executable);
    command
        .arg("--linux-namespace-stage")
        .arg(std::process::id().to_string())
        .current_dir("/")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            let _ = layout.cleanup();
            return SandboxResult::failure(prepared.operation_id, SandboxStatus::LaunchFailed);
        }
    };
    let Some(mut stage_input) = child.stdin.take() else {
        return stop_outer_setup_failure(prepared.operation_id, child, &layout);
    };
    if write_request(&mut stage_input, &request).is_err() {
        drop(stage_input);
        return stop_outer_setup_failure(prepared.operation_id, child, &layout);
    }
    drop(stage_input);
    let Some(group) = Pid::from_raw(i32::try_from(child.id()).unwrap_or_default()) else {
        return stop_outer_setup_failure(prepared.operation_id, child, &layout);
    };
    let Some(stdout) = child.stdout.take() else {
        return stop_outer_setup_failure(prepared.operation_id, child, &layout);
    };
    let Some(stderr) = child.stderr.take() else {
        return stop_outer_setup_failure(prepared.operation_id, child, &layout);
    };

    let output_exceeded = Arc::new(AtomicBool::new(false));
    let stdout_reader = capture_stream(
        stdout,
        prepared.output_bytes_per_stream,
        Arc::clone(&output_exceeded),
    );
    let stderr_reader = capture_stream(
        stderr,
        prepared.output_bytes_per_stream,
        Arc::clone(&output_exceeded),
    );
    let deadline = Instant::now() + Duration::from_millis(prepared.wall_time_milliseconds);
    let terminal = loop {
        if cancellation.is_cancelled() {
            break Err(SandboxStatus::Cancelled);
        }
        if output_exceeded.load(Ordering::Acquire) {
            break Err(SandboxStatus::OutputLimit);
        }
        if Instant::now() >= deadline {
            break Err(SandboxStatus::TimedOut);
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(_) => break Err(SandboxStatus::ProcessTreeUncertain),
        }
    };

    match terminal {
        Ok(stage_status) => {
            let stdout = join_stream(stdout_reader);
            let stderr = join_stream(stderr_reader);
            if output_exceeded.load(Ordering::Acquire) {
                let _ = layout.cleanup();
                return SandboxResult::failure(prepared.operation_id, SandboxStatus::OutputLimit);
            }
            let outcome = read_stage_outcome(&layout.outcome, prepared.operation_id);
            let ready = fs::read(&layout.ready).is_ok_and(|bytes| bytes == READY_BYTES);
            let resource_limited =
                fs::read(&layout.failure).is_ok_and(|bytes| bytes == RESOURCE_LIMIT_BYTES);
            let cleaned = layout.cleanup().is_ok();
            if !stage_status.success() || !ready || !cleaned {
                return SandboxResult::failure(
                    prepared.operation_id,
                    if resource_limited {
                        SandboxStatus::ResourceLimit
                    } else if ready {
                        SandboxStatus::LaunchFailed
                    } else {
                        SandboxStatus::BackendUnavailable
                    },
                );
            }
            let Ok(outcome) = outcome else {
                return SandboxResult::failure(prepared.operation_id, SandboxStatus::LaunchFailed);
            };
            let (status, candidate_eligible) = if outcome.code.is_some() {
                (SandboxStatus::Exited, true)
            } else {
                (SandboxStatus::Signalled, false)
            };
            SandboxResult {
                protocol_version: SANDBOX_PROTOCOL_VERSION,
                operation_id: prepared.operation_id,
                status,
                exit: Some(SandboxExit {
                    code: outcome.code,
                    signal: outcome.signal,
                }),
                stdout,
                stderr,
                candidate_eligible,
            }
        }
        Err(status) => {
            let stopped = terminate_outer_group(&mut child, group);
            let _ = join_stream(stdout_reader);
            let _ = join_stream(stderr_reader);
            if stopped {
                let _ = layout.cleanup();
            }
            SandboxResult::failure(
                prepared.operation_id,
                if stopped {
                    status
                } else {
                    SandboxStatus::ProcessTreeUncertain
                },
            )
        }
    }
}

pub fn run_namespace_stage(request: SandboxRequest, expected_parent: u32) -> ExitCode {
    if bind_to_parent(expected_parent).is_err() {
        return ExitCode::from(STAGE_FAILURE);
    }
    let prepared = match validate_request(&request) {
        Ok(prepared) => prepared,
        Err(()) => return ExitCode::from(STAGE_FAILURE),
    };
    if Layout::existing(&prepared).is_err() {
        return ExitCode::from(STAGE_FAILURE);
    }
    let before_user = match namespace_identity("user") {
        Ok(identity) => identity,
        Err(()) => return ExitCode::from(STAGE_FAILURE),
    };
    let before_mount = match namespace_identity("mnt") {
        Ok(identity) => identity,
        Err(()) => return ExitCode::from(STAGE_FAILURE),
    };
    let before_network = match namespace_identity("net") {
        Ok(identity) => identity,
        Err(()) => return ExitCode::from(STAGE_FAILURE),
    };
    if create_namespaces(before_user, before_mount, before_network).is_err()
        || bind_to_parent(expected_parent).is_err()
    {
        return ExitCode::from(STAGE_FAILURE);
    }

    let executable = match trusted_current_executable() {
        Ok(executable) => executable,
        Err(()) => return ExitCode::from(STAGE_FAILURE),
    };
    let layout = match Layout::existing(&prepared) {
        Ok(layout) => layout,
        Err(()) => return ExitCode::from(STAGE_FAILURE),
    };
    if setup_mounts(&prepared, &layout, &executable).is_err() {
        return ExitCode::from(STAGE_FAILURE);
    }
    let mut child = match Command::new(executable)
        .arg("--linux-pid-stage")
        .current_dir("/")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return ExitCode::from(STAGE_FAILURE),
    };
    let Some(mut input) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return ExitCode::from(STAGE_FAILURE);
    };
    if write_request(&mut input, &request).is_err() {
        drop(input);
        let _ = child.kill();
        let _ = child.wait();
        return ExitCode::from(STAGE_FAILURE);
    }
    let status = child.wait();
    drop(input);
    match status {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(_) | Err(_) => ExitCode::from(STAGE_FAILURE),
    }
}

pub fn run_pid_stage(request: SandboxRequest) -> ExitCode {
    if rustix::process::getpid().as_raw_nonzero().get() != 1
        || set_parent_process_death_signal(Some(Signal::KILL)).is_err()
        || rustix::process::parent_process_death_signal() != Ok(Some(Signal::KILL))
    {
        return ExitCode::from(STAGE_FAILURE);
    }
    let prepared = match validate_request(&request) {
        Ok(prepared) => prepared,
        Err(()) => return ExitCode::from(STAGE_FAILURE),
    };
    let layout = match Layout::existing(&prepared) {
        Ok(layout) => layout,
        Err(()) => return ExitCode::from(STAGE_FAILURE),
    };
    if mount_proc(&layout.root).is_err() {
        return ExitCode::from(STAGE_FAILURE);
    }
    if rustix::process::chroot(&layout.root).is_err()
        || rustix::process::chdir("/").is_err()
        || drop_capabilities().is_err()
    {
        return ExitCode::from(STAGE_FAILURE);
    }

    let cancelled = Arc::new(AtomicBool::new(false));
    let watchdog_cancelled = Arc::clone(&cancelled);
    thread::spawn(move || {
        let mut byte = [0_u8; 1];
        let _ = std::io::stdin().read(&mut byte);
        watchdog_cancelled.store(true, Ordering::Release);
    });

    let mut child = match Command::new("/runner")
        .arg("--linux-command-stage")
        .arg("1")
        .current_dir("/")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .process_group(0)
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return ExitCode::from(STAGE_FAILURE),
    };
    let Some(mut input) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return ExitCode::from(STAGE_FAILURE);
    };
    if write_request(&mut input, &request).is_err() {
        drop(input);
        let _ = child.kill();
        let _ = child.wait();
        return ExitCode::from(STAGE_FAILURE);
    }
    drop(input);
    let Some(group) = Pid::from_raw(i32::try_from(child.id()).unwrap_or_default()) else {
        let _ = child.kill();
        let _ = child.wait();
        return ExitCode::from(STAGE_FAILURE);
    };
    let status = loop {
        if cancelled.load(Ordering::Acquire) {
            let _ = terminate_inner_group(&mut child, group);
            return ExitCode::from(STAGE_FAILURE);
        }
        match namespace_task_count() {
            Ok(count) if count > MAX_SANDBOX_TASKS => {
                let _ = terminate_inner_group(&mut child, group);
                let _ = write_stage_marker("/.morons-failure", RESOURCE_LIMIT_BYTES);
                return ExitCode::from(STAGE_FAILURE);
            }
            Ok(_) => {}
            Err(()) => {
                let _ = terminate_inner_group(&mut child, group);
                return ExitCode::from(STAGE_FAILURE);
            }
        }
        match child_exited_without_reaping(group) {
            Ok(true) => break terminate_inner_group(&mut child, group),
            Ok(false) => thread::sleep(POLL_INTERVAL),
            Err(()) => {
                let _ = terminate_inner_group(&mut child, group);
                return ExitCode::from(STAGE_FAILURE);
            }
        }
    };
    let Ok(status) = status else {
        return ExitCode::from(STAGE_FAILURE);
    };
    if status
        .signal()
        .is_some_and(|signal| signal == libc::SIGXCPU || signal == libc::SIGXFSZ)
    {
        let _ = write_stage_marker("/.morons-failure", RESOURCE_LIMIT_BYTES);
        return ExitCode::from(STAGE_FAILURE);
    }
    if !fs::read("/.morons-ready").is_ok_and(|bytes| bytes == READY_BYTES)
        || write_stage_outcome("/.morons-outcome", request.operation_id, status).is_err()
    {
        return ExitCode::from(STAGE_FAILURE);
    }
    ExitCode::SUCCESS
}

pub fn run_command_stage(request: SandboxRequest, expected_parent: u32) -> ExitCode {
    if bind_to_parent(expected_parent).is_err()
        || rustix::process::getpid().as_raw_nonzero().get() == 1
    {
        return ExitCode::from(STAGE_FAILURE);
    }
    let executable = Path::new("/image").join(&request.executable);
    let working_directory = if request.working_directory == "." {
        PathBuf::from("/workspace")
    } else {
        Path::new("/workspace").join(&request.working_directory)
    };
    if validate_synthetic_target(&executable, false).is_err()
        || validate_synthetic_target(&working_directory, true).is_err()
    {
        return ExitCode::from(STAGE_FAILURE);
    }
    let mut marker = match OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open("/.morons-ready")
    {
        Ok(marker) => marker,
        Err(_) => return ExitCode::from(STAGE_FAILURE),
    };
    if apply_limits(request.limits.wall_time_milliseconds).is_err()
        || verify_capabilities_dropped().is_err()
        || set_no_new_privs(true).is_err()
        || no_new_privs() != Ok(true)
    {
        let _ = write_failed_marker(&mut marker);
        return ExitCode::from(STAGE_FAILURE);
    }
    if landlock::restrict().is_err() {
        let _ = write_failed_marker(&mut marker);
        return ExitCode::from(STAGE_FAILURE);
    }
    if seccomp::Filter::build()
        .and_then(seccomp::Filter::apply)
        .is_err()
        || marker.write_all(READY_BYTES).is_err()
        || marker.sync_all().is_err()
    {
        let _ = write_failed_marker(&mut marker);
        return ExitCode::from(STAGE_FAILURE);
    }

    let mut command = Command::new(executable);
    command
        .args(&request.arguments)
        .current_dir(working_directory)
        .env_clear()
        .env("HOME", "/home/morons")
        .env("TMPDIR", "/tmp")
        .env("CARGO_HOME", "/cargo")
        .env("CARGO_NET_OFFLINE", "true")
        .env("PATH", "/image/bin")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let error = command.exec();
    let _ = write_failed_marker(&mut marker);
    drop(error);
    ExitCode::from(STAGE_FAILURE)
}
