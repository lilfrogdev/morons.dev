use std::{
    io::Read,
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};

use super::{MAX_BASH_OUTPUT_BYTES, ToolErrorKind, ToolInput, ToolOutput, ToolResult};

#[cfg(windows)]
macro_rules! platform_job {
    ($job:ident) => {
        Some($job)
    };
}

#[cfg(not(windows))]
macro_rules! platform_job {
    ($job:ident) => {
        ()
    };
}

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const WALL_TIME_LIMIT: Duration = Duration::from_secs(5 * 60);
const INACTIVITY_LIMIT: Duration = Duration::from_secs(60);
#[cfg(unix)]
const TREE_TERMINATION_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(target_os = "macos")]
const MAX_PROCESS_SNAPSHOT_BYTES: u64 = 64 * 1024;

pub(crate) struct BashToolExecutor {
    working_directory: PathBuf,
}

impl BashToolExecutor {
    pub(crate) fn new(working_directory: PathBuf) -> Self {
        Self { working_directory }
    }

    pub(crate) fn execute<F>(&self, input: &ToolInput, cancelled: &F) -> ToolResult
    where
        F: Fn() -> bool,
    {
        let ToolInput::Bash { command } = input else {
            return ToolResult::error(ToolErrorKind::Filesystem);
        };
        if cancelled() {
            return ToolResult::error(ToolErrorKind::Cancelled);
        }
        self.execute_command(command, cancelled)
    }

    fn execute_command<F>(&self, source: &str, cancelled: &F) -> ToolResult
    where
        F: Fn() -> bool,
    {
        let mut command = Command::new(configured_bash());
        command
            .arg("--noprofile")
            .arg("--norc")
            .arg("-c")
            .arg(source)
            .current_dir(&self.working_directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            command.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }

        #[cfg(windows)]
        let job = match fence_windows::KillOnCloseJob::new() {
            Ok(job) => job,
            Err(_) => return ToolResult::error(ToolErrorKind::Filesystem),
        };
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(_) => return ToolResult::error(ToolErrorKind::Filesystem),
        };
        #[cfg(windows)]
        if job.assign(&child).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return ToolResult::error(ToolErrorKind::Uncertain);
        }
        let process_group = child.id();
        let Some(stdout) = child.stdout.take() else {
            return stop_after_setup_failure(child, process_group, platform_job!(job));
        };
        let Some(stderr) = child.stderr.take() else {
            return stop_after_setup_failure(child, process_group, platform_job!(job));
        };

        let activity = Arc::new(Mutex::new(Instant::now()));
        let output_exceeded = Arc::new(AtomicBool::new(false));
        let capture_failed = Arc::new(AtomicBool::new(false));
        let stdout_reader = capture(
            stdout,
            Arc::clone(&activity),
            Arc::clone(&output_exceeded),
            Arc::clone(&capture_failed),
        );
        let stderr_reader = capture(
            stderr,
            Arc::clone(&activity),
            Arc::clone(&output_exceeded),
            Arc::clone(&capture_failed),
        );
        let deadline = Instant::now() + WALL_TIME_LIMIT;

        let terminal = loop {
            if cancelled() {
                break CommandTerminal::Stopped(ToolErrorKind::Cancelled);
            }
            if output_exceeded.load(Ordering::Acquire) {
                break CommandTerminal::Stopped(ToolErrorKind::OutputLimit);
            }
            if capture_failed.load(Ordering::Acquire) {
                break CommandTerminal::Stopped(ToolErrorKind::Uncertain);
            }
            let now = Instant::now();
            if now >= deadline {
                break CommandTerminal::Stopped(ToolErrorKind::TimedOut);
            }
            if activity
                .lock()
                .map_or(true, |last| now.duration_since(*last) >= INACTIVITY_LIMIT)
            {
                break CommandTerminal::Stopped(ToolErrorKind::InactivityTimeout);
            }
            match child.try_wait() {
                Ok(Some(status)) => break CommandTerminal::Exited(status),
                Ok(None) => thread::sleep(POLL_INTERVAL),
                Err(_) => break CommandTerminal::Stopped(ToolErrorKind::Uncertain),
            }
        };

        let (status, stop_error) = match terminal {
            CommandTerminal::Exited(status) => (Some(status), None),
            CommandTerminal::Stopped(error) => (None, Some(error)),
        };
        let stopped = terminate_tree(
            &mut child,
            process_group,
            status.is_some(),
            platform_job!(job),
        );
        let stdout = join_capture(stdout_reader);
        let stderr = join_capture(stderr_reader);
        if !stopped {
            return ToolResult::error_with_output(
                ToolErrorKind::Uncertain,
                command_output(status.as_ref(), stdout, stderr),
            );
        }
        let output = command_output(status.as_ref(), stdout, stderr);
        match stop_error {
            Some(error) => ToolResult::error_with_output(error, output),
            None => ToolResult::Ok { output },
        }
    }
}

#[cfg(windows)]
type PlatformJob = Option<fence_windows::KillOnCloseJob>;
#[cfg(not(windows))]
type PlatformJob = ();

fn configured_bash() -> std::ffi::OsString {
    if let Some(configured) = std::env::var_os("MORONS_BASH").filter(|value| !value.is_empty()) {
        return configured;
    }
    #[cfg(windows)]
    for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(root) = std::env::var_os(variable) {
            for relative in ["Git/bin/bash.exe", "Git/usr/bin/bash.exe"] {
                let candidate = PathBuf::from(&root).join(relative);
                if candidate.is_file() {
                    return candidate.into_os_string();
                }
            }
        }
    }
    "bash".into()
}

fn stop_after_setup_failure(mut child: Child, process_group: u32, job: PlatformJob) -> ToolResult {
    if terminate_tree(&mut child, process_group, false, job) {
        ToolResult::error(ToolErrorKind::Filesystem)
    } else {
        ToolResult::error(ToolErrorKind::Uncertain)
    }
}

enum CommandTerminal {
    Exited(ExitStatus),
    Stopped(ToolErrorKind),
}

fn capture<R: Read + Send + 'static>(
    mut reader: R,
    activity: Arc<Mutex<Instant>>,
    exceeded: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut output = Vec::with_capacity(MAX_BASH_OUTPUT_BYTES.min(8 * 1024));
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            let read = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(_) => {
                    failed.store(true, Ordering::Release);
                    break;
                }
            };
            if let Ok(mut last) = activity.lock() {
                *last = Instant::now();
            } else {
                failed.store(true, Ordering::Release);
                break;
            }
            let remaining = MAX_BASH_OUTPUT_BYTES.saturating_sub(output.len());
            output.extend_from_slice(&buffer[..read.min(remaining)]);
            if read > remaining {
                exceeded.store(true, Ordering::Release);
            }
        }
        output
    })
}

fn join_capture(handle: thread::JoinHandle<Vec<u8>>) -> String {
    let bytes = handle.join().unwrap_or_default();
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if text.len() > MAX_BASH_OUTPUT_BYTES {
        let mut boundary = MAX_BASH_OUTPUT_BYTES;
        while !text.is_char_boundary(boundary) {
            boundary -= 1;
        }
        text.truncate(boundary);
    }
    text
}

fn command_output(status: Option<&ExitStatus>, stdout: String, stderr: String) -> ToolOutput {
    ToolOutput::Bash {
        exit_code: status.and_then(ExitStatus::code),
        signal: exit_signal(status),
        stdout,
        stderr,
    }
}

#[cfg(unix)]
fn exit_signal(status: Option<&ExitStatus>) -> Option<u16> {
    status
        .and_then(|status| status.signal())
        .and_then(|signal| u16::try_from(signal).ok())
}

#[cfg(not(unix))]
fn exit_signal(_status: Option<&ExitStatus>) -> Option<u16> {
    None
}

#[cfg(unix)]
fn terminate_tree(
    child: &mut Child,
    process_group: u32,
    already_reaped: bool,
    _job: PlatformJob,
) -> bool {
    use rustix::{
        io::Errno,
        process::{Pid, Signal, kill_process_group},
    };

    let Some(group) = i32::try_from(process_group).ok().and_then(Pid::from_raw) else {
        let _ = child.kill();
        return child.wait().is_ok();
    };
    let group_missing = match kill_process_group(group, Signal::KILL) {
        Ok(()) => false,
        Err(Errno::SRCH) => true,
        Err(_) => return false,
    };
    if !already_reaped && child.wait().is_err() {
        return false;
    }
    if group_missing {
        return true;
    }
    let deadline = Instant::now() + TREE_TERMINATION_TIMEOUT;
    loop {
        match process_group_has_live_members(group) {
            Ok(false) => return true,
            Ok(true) if Instant::now() < deadline => {
                match kill_process_group(group, Signal::KILL) {
                    Ok(()) | Err(Errno::SRCH) => thread::sleep(POLL_INTERVAL),
                    Err(_) => return false,
                }
            }
            Ok(true) | Err(()) => return false,
        }
    }
}

#[cfg(target_os = "linux")]
fn process_group_has_live_members(group: rustix::process::Pid) -> Result<bool, ()> {
    let group = group.as_raw_nonzero().get();
    for entry in std::fs::read_dir("/proc").map_err(|_| ())? {
        let entry = entry.map_err(|_| ())?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let stat = match std::fs::read(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(()),
        };
        if stat.len() > 4_096 {
            return Err(());
        }
        let stat = std::str::from_utf8(&stat).map_err(|_| ())?;
        let close = stat.rfind(')').ok_or(())?;
        let mut fields = stat.get(close + 1..).ok_or(())?.split_ascii_whitespace();
        let state = fields.next().ok_or(())?;
        let _parent = fields.next().ok_or(())?;
        let process_group = fields.next().ok_or(())?.parse::<i32>().map_err(|_| ())?;
        if process_group == group && !matches!(state, "Z" | "X") {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "macos")]
fn process_group_has_live_members(group: rustix::process::Pid) -> Result<bool, ()> {
    let group = group.as_raw_nonzero().get().to_string();
    let mut child = Command::new("/bin/ps")
        .args(["-o", "pgid=", "-o", "state=", "-g", &group])
        .env_clear()
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ())?;
    let mut output = Vec::new();
    child
        .stdout
        .take()
        .ok_or(())?
        .take(MAX_PROCESS_SNAPSHOT_BYTES + 1)
        .read_to_end(&mut output)
        .map_err(|_| ())?;
    let status = child.wait().map_err(|_| ())?;
    if status.code() == Some(1) && output.is_empty() {
        return Ok(false);
    }
    if !status.success() || output.len() as u64 > MAX_PROCESS_SNAPSHOT_BYTES {
        return Err(());
    }
    let output = std::str::from_utf8(&output).map_err(|_| ())?;
    for line in output.lines() {
        let mut fields = line.split_ascii_whitespace();
        let process_group = fields.next().ok_or(())?;
        let state = fields.next().ok_or(())?;
        if fields.next().is_some() || process_group != group || state.is_empty() {
            return Err(());
        }
        if !state.starts_with('Z') {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn process_group_has_live_members(_group: rustix::process::Pid) -> Result<bool, ()> {
    Err(())
}

#[cfg(windows)]
fn terminate_tree(
    child: &mut Child,
    _process_group: u32,
    already_reaped: bool,
    job: PlatformJob,
) -> bool {
    drop(job);
    already_reaped || child.wait().is_ok()
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::AtomicBool,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::tools::ToolInput;

    #[test]
    fn bash_runs_in_selected_directory_with_closed_stdin_and_separate_output() {
        let root = test_directory("complete");
        let executor = BashToolExecutor::new(root.clone());
        let result = executor.execute(
            &ToolInput::Bash {
                command: "printf 'out'; printf 'err' >&2; if read -r value; then exit 9; fi"
                    .to_owned(),
            },
            &|| false,
        );
        assert!(matches!(
            result,
            ToolResult::Ok {
                output: ToolOutput::Bash {
                    exit_code: Some(0),
                    ref stdout,
                    ref stderr,
                    ..
                }
            } if stdout == "out" && stderr == "err"
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn output_exhaustion_is_bounded_and_stops_the_tree() {
        let root = test_directory("output-limit");
        let executor = BashToolExecutor::new(root.clone());
        let result = executor.execute(
            &ToolInput::Bash {
                command: "while :; do printf 1234567890; done".to_owned(),
            },
            &|| false,
        );
        assert!(matches!(
            result,
            ToolResult::Error {
                error: ToolErrorKind::OutputLimit,
                output: Some(ToolOutput::Bash { ref stdout, .. }),
            } if stdout.len() <= MAX_BASH_OUTPUT_BYTES
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancellation_stops_background_descendants() {
        let root = test_directory("cancel");
        let marker = root.join("leaked");
        let executor = BashToolExecutor::new(root.clone());
        let cancelled = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancelled);
        let cancellation = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            trigger.store(true, Ordering::Release);
        });
        let result = executor.execute(
            &ToolInput::Bash {
                command: "(sleep 2; printf leaked > leaked) & wait".to_owned(),
            },
            &|| cancelled.load(Ordering::Acquire),
        );
        cancellation.join().unwrap();
        assert!(matches!(
            result,
            ToolResult::Error {
                error: ToolErrorKind::Cancelled,
                ..
            }
        ));
        thread::sleep(Duration::from_millis(300));
        assert!(!marker.exists());
        fs::remove_dir_all(root).unwrap();
    }

    fn test_directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "morons-bash-tool-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        path
    }
}
