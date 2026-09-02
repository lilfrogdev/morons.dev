mod staging;

use std::{
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use morons_windows_native::{
    CommandCompletion, CommandLaunch, CommandLimits, CommandProcess, OperationPaths,
    OperationProfile,
};

use crate::{
    Cancellation, SANDBOX_PROTOCOL_VERSION, SandboxExit, SandboxRequest, SandboxResult,
    SandboxStatus, runner::PreparedRequest,
};
use staging::Layout;

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const TREE_TERMINATION_TIMEOUT: Duration = Duration::from_secs(2);
const JOB_MEMORY_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const JOB_PROCESS_COUNT: u32 = 128;

pub(crate) fn execute(
    prepared: PreparedRequest,
    _request: SandboxRequest,
    cancellation: &Cancellation,
) -> SandboxResult {
    let operation_id = prepared.operation_id;
    let layout = match Layout::prepare(&prepared) {
        Ok(layout) => layout,
        Err(()) => return SandboxResult::failure(operation_id, SandboxStatus::LaunchFailed),
    };
    let profile = match OperationProfile::create(operation_id) {
        Ok(profile) => profile,
        Err(_) => {
            let _ = layout.cleanup();
            return SandboxResult::failure(operation_id, SandboxStatus::BackendUnavailable);
        }
    };
    if profile
        .grant_operation(OperationPaths {
            candidate: &prepared.candidate_root,
            runtime: &layout.runtime,
            image: &layout.image,
        })
        .is_err()
    {
        return cleanup_failure(
            operation_id,
            SandboxStatus::BackendUnavailable,
            profile,
            &layout,
        );
    }
    let mut process = match profile.launch_command(CommandLaunch {
        executable: &layout.executable,
        arguments: &prepared.arguments,
        candidate: &prepared.candidate_root,
        working_directory: &prepared.working_directory,
        runtime: &layout.runtime,
        image: &layout.image,
        limits: CommandLimits {
            memory_bytes: JOB_MEMORY_BYTES,
            process_count: JOB_PROCESS_COUNT,
        },
    }) {
        Ok(process) => process,
        Err(_) => {
            return cleanup_failure(operation_id, SandboxStatus::LaunchFailed, profile, &layout);
        }
    };
    let Some(stdout) = process.take_stdout() else {
        return stop_and_cleanup(
            operation_id,
            SandboxStatus::LaunchFailed,
            process,
            profile,
            &layout,
        );
    };
    let Some(stderr) = process.take_stderr() else {
        return stop_and_cleanup(
            operation_id,
            SandboxStatus::LaunchFailed,
            process,
            profile,
            &layout,
        );
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
        match process.wait_root(Duration::ZERO) {
            Ok(Some(_)) => break Ok(()),
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(_) => break Err(SandboxStatus::ProcessTreeUncertain),
        }
    };

    match terminal {
        Ok(()) => complete(
            operation_id,
            process,
            profile,
            &layout,
            stdout_reader,
            stderr_reader,
            &output_exceeded,
        ),
        Err(status) => {
            let stopped = process
                .terminate_and_verify(TREE_TERMINATION_TIMEOUT)
                .is_ok();
            let _ = join_stream(stdout_reader);
            let _ = join_stream(stderr_reader);
            if !stopped {
                return SandboxResult::failure(operation_id, SandboxStatus::ProcessTreeUncertain);
            }
            cleanup_failure(operation_id, status, profile, &layout)
        }
    }
}

fn complete(
    operation_id: [u8; 16],
    process: CommandProcess,
    profile: OperationProfile,
    layout: &Layout,
    stdout_reader: thread::JoinHandle<Vec<u8>>,
    stderr_reader: thread::JoinHandle<Vec<u8>>,
    output_exceeded: &AtomicBool,
) -> SandboxResult {
    let completion = process.complete_and_verify(TREE_TERMINATION_TIMEOUT);
    let stdout = join_stream(stdout_reader);
    let stderr = join_stream(stderr_reader);
    let completion = match completion {
        Ok(completion) => completion,
        Err(_) => {
            return SandboxResult::failure(operation_id, SandboxStatus::ProcessTreeUncertain);
        }
    };
    if output_exceeded.load(Ordering::Acquire) {
        return cleanup_failure(operation_id, SandboxStatus::OutputLimit, profile, layout);
    }
    let CommandCompletion::Clean { exit_code } = completion else {
        return cleanup_failure(operation_id, SandboxStatus::ResourceLimit, profile, layout);
    };
    if !delete_and_cleanup(profile, layout) {
        return SandboxResult::failure(operation_id, SandboxStatus::BackendUnavailable);
    }
    let code = exit_code as i32;
    let crashed = code < 0;
    SandboxResult {
        protocol_version: SANDBOX_PROTOCOL_VERSION,
        operation_id,
        status: if crashed {
            SandboxStatus::Crashed
        } else {
            SandboxStatus::Exited
        },
        exit: Some(SandboxExit {
            code: Some(code),
            signal: None,
        }),
        stdout,
        stderr,
        candidate_eligible: !crashed,
    }
}

fn stop_and_cleanup(
    operation_id: [u8; 16],
    status: SandboxStatus,
    process: CommandProcess,
    profile: OperationProfile,
    layout: &Layout,
) -> SandboxResult {
    if process
        .terminate_and_verify(TREE_TERMINATION_TIMEOUT)
        .is_err()
    {
        return SandboxResult::failure(operation_id, SandboxStatus::ProcessTreeUncertain);
    }
    cleanup_failure(operation_id, status, profile, layout)
}

fn cleanup_failure(
    operation_id: [u8; 16],
    status: SandboxStatus,
    profile: OperationProfile,
    layout: &Layout,
) -> SandboxResult {
    SandboxResult::failure(
        operation_id,
        if delete_and_cleanup(profile, layout) {
            status
        } else {
            SandboxStatus::BackendUnavailable
        },
    )
}

fn delete_and_cleanup(profile: OperationProfile, layout: &Layout) -> bool {
    profile.delete().is_ok() && layout.cleanup().is_ok()
}

fn capture_stream<R: Read + Send + 'static>(
    mut reader: R,
    maximum: usize,
    exceeded: Arc<AtomicBool>,
) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut output = Vec::with_capacity(maximum.min(8 * 1024));
        let mut buffer = [0u8; 8 * 1024];
        loop {
            let read = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(_) => {
                    exceeded.store(true, Ordering::Release);
                    break;
                }
            };
            let Some(next) = output.len().checked_add(read) else {
                exceeded.store(true, Ordering::Release);
                continue;
            };
            if next > maximum {
                exceeded.store(true, Ordering::Release);
                continue;
            }
            output.extend_from_slice(&buffer[..read]);
        }
        output
    })
}

fn join_stream(handle: thread::JoinHandle<Vec<u8>>) -> Vec<u8> {
    handle.join().unwrap_or_default()
}
