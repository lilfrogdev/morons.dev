use std::{
    io::Read,
    os::windows::process::CommandExt,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    Cancellation, SANDBOX_PROTOCOL_VERSION, SandboxExit, SandboxRequest, SandboxResult,
    SandboxStatus, runner::validate_request,
};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub(super) fn execute(request: SandboxRequest, cancellation: &Cancellation) -> SandboxResult {
    let operation_id = request.operation_id;
    let prepared = match validate_request(&request) {
        Ok(prepared) => prepared,
        Err(()) => {
            diagnostic("direct-request");
            return SandboxResult::failure(operation_id, SandboxStatus::RequestRejected);
        }
    };
    for directory in [
        prepared.scratch_root.join("home"),
        prepared.scratch_root.join("tmp"),
        prepared.scratch_root.join("cargo-home"),
    ] {
        if !directory.is_dir() {
            diagnostic("direct-directories");
            return SandboxResult::failure(operation_id, SandboxStatus::RequestRejected);
        }
    }
    if cancellation.is_cancelled() {
        return SandboxResult::failure(operation_id, SandboxStatus::Cancelled);
    }

    let mut command = Command::new(&prepared.executable);
    command
        .args(&prepared.arguments)
        .current_dir(&prepared.working_directory)
        .env_clear()
        .envs(match target_environment(&prepared) {
            Ok(environment) => environment,
            Err(()) => {
                diagnostic("direct-environment");
                return SandboxResult::failure(operation_id, SandboxStatus::BackendUnavailable);
            }
        })
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            diagnostic("direct-launch");
            return SandboxResult::failure(operation_id, SandboxStatus::LaunchFailed);
        }
    };
    let Some(stdout) = child.stdout.take() else {
        return stop_after_setup_failure(operation_id, &mut child);
    };
    let Some(stderr) = child.stderr.take() else {
        return stop_after_setup_failure(operation_id, &mut child);
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
        Ok(status) => complete(
            operation_id,
            status,
            stdout_reader,
            stderr_reader,
            &output_exceeded,
        ),
        Err(status) => {
            let stopped = stop_root(&mut child);
            let _ = join_stream(stdout_reader);
            let _ = join_stream(stderr_reader);
            SandboxResult::failure(
                operation_id,
                if stopped {
                    status
                } else {
                    SandboxStatus::ProcessTreeUncertain
                },
            )
        }
    }
}

fn complete(
    operation_id: [u8; 16],
    status: ExitStatus,
    stdout_reader: thread::JoinHandle<Vec<u8>>,
    stderr_reader: thread::JoinHandle<Vec<u8>>,
    output_exceeded: &AtomicBool,
) -> SandboxResult {
    let stdout = join_stream(stdout_reader);
    let stderr = join_stream(stderr_reader);
    if output_exceeded.load(Ordering::Acquire) {
        return SandboxResult::failure(operation_id, SandboxStatus::OutputLimit);
    }
    let Some(code) = status.code() else {
        return SandboxResult::failure(operation_id, SandboxStatus::ProcessTreeUncertain);
    };
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

fn target_environment(
    prepared: &crate::runner::PreparedRequest,
) -> Result<Vec<(String, String)>, ()> {
    let home = utf8(&prepared.scratch_root.join("home"))?;
    let temporary = utf8(&prepared.scratch_root.join("tmp"))?;
    let cargo_home = utf8(&prepared.scratch_root.join("cargo-home"))?;
    let image_path = utf8(&prepared.image_root.join("bin"))?;
    let mut environment = Vec::new();
    for name in ["SystemRoot", "windir", "SystemDrive", "ComSpec", "PATHEXT"] {
        let value = std::env::var(name).map_err(|_| ())?;
        environment.push((name.to_owned(), value));
    }
    for name in [
        "OS",
        "PROCESSOR_ARCHITECTURE",
        "PROCESSOR_IDENTIFIER",
        "PROCESSOR_LEVEL",
        "PROCESSOR_REVISION",
        "NUMBER_OF_PROCESSORS",
    ] {
        if let Ok(value) = std::env::var(name) {
            environment.push((name.to_owned(), value));
        }
    }
    environment.extend([
        ("HOME".to_owned(), home.clone()),
        ("USERPROFILE".to_owned(), home.clone()),
        ("LOCALAPPDATA".to_owned(), home.clone()),
        ("APPDATA".to_owned(), home),
        ("TEMP".to_owned(), temporary.clone()),
        ("TMP".to_owned(), temporary),
        ("CARGO_HOME".to_owned(), cargo_home),
        ("CARGO_NET_OFFLINE".to_owned(), "true".to_owned()),
        ("PATH".to_owned(), image_path),
        ("TERM".to_owned(), "dumb".to_owned()),
        ("NO_COLOR".to_owned(), "1".to_owned()),
    ]);
    Ok(environment)
}

fn stop_after_setup_failure(operation_id: [u8; 16], child: &mut Child) -> SandboxResult {
    SandboxResult::failure(
        operation_id,
        if stop_root(child) {
            SandboxStatus::LaunchFailed
        } else {
            SandboxStatus::ProcessTreeUncertain
        },
    )
}

fn stop_root(child: &mut Child) -> bool {
    match child.kill() {
        Ok(()) => child.wait().is_ok(),
        Err(_) => child.try_wait().is_ok_and(|status| status.is_some()),
    }
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

fn join_stream(handle: thread::JoinHandle<Vec<u8>>) -> Vec<u8> {
    handle.join().unwrap_or_default()
}

fn diagnostic(stage: &'static str) {
    if std::env::var_os("MORONS_SANDBOX_TEST_DIAGNOSTICS").is_some() {
        eprintln!("windows sandbox stage: {stage}");
    }
}

fn utf8(path: &std::path::Path) -> Result<String, ()> {
    path.to_str().map(str::to_owned).ok_or(())
}
