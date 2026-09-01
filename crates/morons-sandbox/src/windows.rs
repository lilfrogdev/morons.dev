mod direct;
mod staging;

use std::{
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use rappct::{
    JobLimits, LaunchOptions, StdioConfig,
    launch::{LaunchedIo, launch_in_container_with_io},
};

use crate::{
    Cancellation, SandboxRequest, SandboxResult, SandboxStatus, read_result,
    runner::PreparedRequest, write_request,
};

use staging::{Container, Layout, bootstrap_environment, command_line};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const TREE_TERMINATION_TIMEOUT: Duration = Duration::from_secs(2);
const BOOTSTRAP_STDERR_BYTES: usize = 64 * 1024;
const PROCESS_MEMORY_BYTES: usize = 2 * 1024 * 1024 * 1024;

pub(crate) fn execute(
    prepared: PreparedRequest,
    request: SandboxRequest,
    cancellation: &Cancellation,
) -> SandboxResult {
    let operation_id = prepared.operation_id;
    let layout = match Layout::prepare(&prepared) {
        Ok(layout) => layout,
        Err(()) => return SandboxResult::failure(operation_id, SandboxStatus::LaunchFailed),
    };
    let stage_request = match layout.stage_request(&prepared, &request) {
        Ok(request) => request,
        Err(()) => {
            let _ = layout.cleanup();
            return SandboxResult::failure(operation_id, SandboxStatus::RequestRejected);
        }
    };
    let container = match Container::create(operation_id) {
        Ok(container) => container,
        Err(()) => {
            let _ = layout.cleanup();
            return SandboxResult::failure(operation_id, SandboxStatus::BackendUnavailable);
        }
    };
    if container.grant_paths(&prepared, &layout).is_err() {
        return cleanup_failure(
            operation_id,
            SandboxStatus::BackendUnavailable,
            container,
            &layout,
        );
    }
    let capabilities = match container.capabilities() {
        Ok(capabilities) => capabilities,
        Err(()) => {
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
            return cleanup_failure(
                operation_id,
                SandboxStatus::BackendUnavailable,
                container,
                &layout,
            );
        }
    };
    let mut launched = match launch_in_container_with_io(&capabilities, &options) {
        Ok(launched) => launched,
        Err(_) => {
            return cleanup_failure(
                operation_id,
                SandboxStatus::BackendUnavailable,
                container,
                &layout,
            );
        }
    };
    if launched.job_guard.is_none() {
        drop(launched.stdin.take());
        let stopped = launched.wait(Some(TREE_TERMINATION_TIMEOUT)).is_ok();
        if stopped {
            return cleanup_failure(
                operation_id,
                SandboxStatus::BackendUnavailable,
                container,
                &layout,
            );
        }
        return SandboxResult::failure(operation_id, SandboxStatus::ProcessTreeUncertain);
    }
    let Some(mut input) = launched.stdin.take() else {
        return stop_and_cleanup(
            operation_id,
            SandboxStatus::BackendUnavailable,
            launched,
            container,
            &layout,
        );
    };
    let Some(mut stdout) = launched.stdout.take() else {
        drop(input);
        return stop_and_cleanup(
            operation_id,
            SandboxStatus::BackendUnavailable,
            launched,
            container,
            &layout,
        );
    };
    let Some(stderr) = launched.stderr.take() else {
        drop(input);
        return stop_and_cleanup(
            operation_id,
            SandboxStatus::BackendUnavailable,
            launched,
            container,
            &layout,
        );
    };

    let (result_sender, result_receiver) = mpsc::sync_channel(1);
    let result_reader = thread::spawn(move || {
        let _ = result_sender.send(read_result(&mut stdout));
    });
    let stderr_exceeded = Arc::new(AtomicBool::new(false));
    let stderr_reader =
        capture_stream(stderr, BOOTSTRAP_STDERR_BYTES, Arc::clone(&stderr_exceeded));
    if write_request(&mut input, &stage_request).is_err() {
        drop(input);
        let result = stop_and_cleanup(
            operation_id,
            SandboxStatus::BackendUnavailable,
            launched,
            container,
            &layout,
        );
        let _ = result_reader.join();
        let _ = stderr_reader.join();
        return result;
    }

    let deadline = Instant::now()
        + Duration::from_millis(prepared.wall_time_milliseconds)
        + TREE_TERMINATION_TIMEOUT;
    loop {
        if cancellation.is_cancelled() {
            drop(input);
            let result = stop_and_cleanup(
                operation_id,
                SandboxStatus::Cancelled,
                launched,
                container,
                &layout,
            );
            let _ = result_reader.join();
            let _ = stderr_reader.join();
            return result;
        }
        if stderr_exceeded.load(Ordering::Acquire) || Instant::now() >= deadline {
            drop(input);
            let result = stop_and_cleanup(
                operation_id,
                if stderr_exceeded.load(Ordering::Acquire) {
                    SandboxStatus::BackendUnavailable
                } else {
                    SandboxStatus::TimedOut
                },
                launched,
                container,
                &layout,
            );
            let _ = result_reader.join();
            let _ = stderr_reader.join();
            return result;
        }
        match result_receiver.try_recv() {
            Ok(Ok(result)) => {
                drop(input);
                let stopped = stop_job(launched);
                let _ = result_reader.join();
                let stderr = stderr_reader.join().unwrap_or_default();
                if !stopped {
                    return SandboxResult::failure(
                        operation_id,
                        SandboxStatus::ProcessTreeUncertain,
                    );
                }
                let profile_deleted = container.delete().is_ok();
                let staging_removed = layout.cleanup().is_ok();
                let cleaned = profile_deleted && staging_removed;
                if !cleaned
                    || !stderr.is_empty()
                    || stderr_exceeded.load(Ordering::Acquire)
                    || result.operation_id != operation_id
                {
                    return SandboxResult::failure(operation_id, SandboxStatus::BackendUnavailable);
                }
                return result;
            }
            Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                drop(input);
                let result = stop_and_cleanup(
                    operation_id,
                    SandboxStatus::BackendUnavailable,
                    launched,
                    container,
                    &layout,
                );
                let _ = result_reader.join();
                let _ = stderr_reader.join();
                return result;
            }
            Err(mpsc::TryRecvError::Empty) => thread::sleep(POLL_INTERVAL),
        }
    }
}

pub fn run_command_stage(request: SandboxRequest, cancellation: &Cancellation) -> SandboxResult {
    direct::execute(request, cancellation)
}

fn launch_options(prepared: &PreparedRequest, layout: &Layout) -> Result<LaunchOptions, ()> {
    Ok(LaunchOptions {
        exe: layout.runner.clone(),
        cmdline: Some(command_line(&layout.runner, "--windows-command-stage")?),
        cwd: Some(prepared.candidate_root.clone()),
        env: Some(bootstrap_environment(layout)?),
        stdio: StdioConfig::Pipe,
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
    drop(launched.stdin.take());
    let Some(job) = launched.job_guard.take() else {
        return false;
    };
    drop(job);
    launched.wait(Some(TREE_TERMINATION_TIMEOUT)).is_ok()
}

fn cleanup_failure(
    operation_id: [u8; 16],
    status: SandboxStatus,
    container: Container,
    layout: &Layout,
) -> SandboxResult {
    let profile_deleted = container.delete().is_ok();
    let staging_removed = layout.cleanup().is_ok();
    let cleaned = profile_deleted && staging_removed;
    SandboxResult::failure(
        operation_id,
        if cleaned {
            status
        } else {
            SandboxStatus::BackendUnavailable
        },
    )
}

fn capture_stream<R: Read + Send + 'static>(
    mut reader: R,
    maximum: usize,
    exceeded: Arc<AtomicBool>,
) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut output = Vec::with_capacity(maximum.min(8 * 1024));
        let mut buffer = [0_u8; 8 * 1024];
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
