use std::{
    fs,
    io::{self, Read},
    os::unix::process::{CommandExt, ExitStatusExt},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use rustix::{
    io::Errno,
    process::{Pid, Signal, kill_process_group},
};

use crate::{Cancellation, SandboxExit, SandboxResult, SandboxStatus, runner::PreparedRequest};

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
const PS_EXEC: &str = "/bin/ps";
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const TREE_TERMINATION_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_PROCESS_SNAPSHOT_BYTES: usize = 256 * 1024;

pub(crate) fn execute(request: PreparedRequest, cancellation: &Cancellation) -> SandboxResult {
    let profile = match profile(&request) {
        Ok(profile) => profile,
        Err(()) => {
            return SandboxResult::failure(request.operation_id, SandboxStatus::RequestRejected);
        }
    };
    let home = request.scratch_root.join("home");
    let temporary = request.scratch_root.join("tmp");
    let cargo_home = request.scratch_root.join("cargo-home");
    if [home.as_path(), temporary.as_path(), cargo_home.as_path()]
        .into_iter()
        .try_for_each(create_private_directory)
        .is_err()
    {
        return SandboxResult::failure(request.operation_id, SandboxStatus::LaunchFailed);
    }

    let mut command = Command::new(SANDBOX_EXEC);
    command
        .arg("-p")
        .arg(profile)
        .arg(&request.executable)
        .args(&request.arguments)
        .current_dir(&request.working_directory)
        .env_clear()
        .env("HOME", &home)
        .env("TMPDIR", &temporary)
        .env("CARGO_HOME", &cargo_home)
        .env("CARGO_NET_OFFLINE", "true")
        .env("PATH", request.image_root.join("bin"))
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            return SandboxResult::failure(request.operation_id, SandboxStatus::LaunchFailed);
        }
    };
    let Some(group) = Pid::from_raw(i32::try_from(child.id()).unwrap_or_default()) else {
        let _ = child.kill();
        let _ = child.wait();
        return SandboxResult::failure(request.operation_id, SandboxStatus::ProcessTreeUncertain);
    };
    let Some(stdout) = child.stdout.take() else {
        return stop_after_setup_failure(request.operation_id, &mut child, group);
    };
    let Some(stderr) = child.stderr.take() else {
        return stop_after_setup_failure(request.operation_id, &mut child, group);
    };

    let output_exceeded = Arc::new(AtomicBool::new(false));
    let stdout_reader = capture_stream(
        stdout,
        request.output_bytes_per_stream,
        Arc::clone(&output_exceeded),
    );
    let stderr_reader = capture_stream(
        stderr,
        request.output_bytes_per_stream,
        Arc::clone(&output_exceeded),
    );
    let deadline = Instant::now() + Duration::from_millis(request.wall_time_milliseconds);
    let status = loop {
        if cancellation.is_cancelled() {
            break SandboxStatus::Cancelled;
        }
        if output_exceeded.load(Ordering::Acquire) {
            break SandboxStatus::OutputLimit;
        }
        if Instant::now() >= deadline {
            break SandboxStatus::TimedOut;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let tree_stopped = terminate_group(&mut child, group);
                let stdout = join_stream(stdout_reader);
                let stderr = join_stream(stderr_reader);
                if !tree_stopped {
                    return SandboxResult::failure(
                        request.operation_id,
                        SandboxStatus::ProcessTreeUncertain,
                    );
                }
                return SandboxResult {
                    protocol_version: crate::SANDBOX_PROTOCOL_VERSION,
                    operation_id: request.operation_id,
                    status: SandboxStatus::Exited,
                    exit: Some(SandboxExit {
                        code: status.code(),
                        signal: status
                            .signal()
                            .and_then(|signal| u16::try_from(signal).ok()),
                    }),
                    stdout,
                    stderr,
                    candidate_eligible: true,
                };
            }
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(_) => break SandboxStatus::ProcessTreeUncertain,
        }
    };

    let tree_stopped = terminate_group(&mut child, group);
    let _ = join_stream(stdout_reader);
    let _ = join_stream(stderr_reader);
    SandboxResult::failure(
        request.operation_id,
        if tree_stopped {
            status
        } else {
            SandboxStatus::ProcessTreeUncertain
        },
    )
}

fn stop_after_setup_failure(
    operation_id: [u8; 16],
    child: &mut Child,
    group: Pid,
) -> SandboxResult {
    let stopped = terminate_group(child, group);
    SandboxResult::failure(
        operation_id,
        if stopped {
            SandboxStatus::LaunchFailed
        } else {
            SandboxStatus::ProcessTreeUncertain
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

fn join_stream(handle: thread::JoinHandle<Vec<u8>>) -> Vec<u8> {
    handle.join().unwrap_or_default()
}

fn terminate_group(child: &mut Child, group: Pid) -> bool {
    match kill_process_group(group, Signal::KILL) {
        Ok(()) | Err(Errno::SRCH) => {}
        Err(_) => return false,
    }
    if child.wait().is_err() {
        return false;
    }
    let deadline = Instant::now() + TREE_TERMINATION_TIMEOUT;
    loop {
        match process_group_has_live_members(group) {
            Ok(false) => return true,
            Ok(true) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            Ok(true) | Err(()) => return false,
        }
    }
}

fn process_group_has_live_members(group: Pid) -> Result<bool, ()> {
    let group_id = group.as_raw_nonzero().get().to_string();
    let output = Command::new(PS_EXEC)
        .args(["-o", "pid=", "-o", "pgid=", "-o", "state=", "-g", &group_id])
        .env_clear()
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(|_| ())?;
    if output.stdout.len() > MAX_PROCESS_SNAPSHOT_BYTES
        || output.stderr.len() > MAX_PROCESS_SNAPSHOT_BYTES
    {
        return Err(());
    }
    if !output.status.success() {
        return if output.status.code() == Some(1)
            && output.stdout.is_empty()
            && output.stderr.is_empty()
        {
            Ok(false)
        } else {
            Err(())
        };
    }
    if !output.stderr.is_empty() {
        return Err(());
    }
    let snapshot = std::str::from_utf8(&output.stdout).map_err(|_| ())?;
    for line in snapshot.lines() {
        let mut fields = line.split_ascii_whitespace();
        let _process_id = fields.next().ok_or(())?.parse::<u32>().map_err(|_| ())?;
        let process_group = fields.next().ok_or(())?;
        let state = fields.next().ok_or(())?;
        if fields.next().is_some() || process_group != group_id || state.is_empty() {
            return Err(());
        }
        if !state.starts_with('Z') {
            return Ok(true);
        }
    }
    Ok(false)
}

fn create_private_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    if !path.exists() {
        let mut builder = fs::DirBuilder::new();
        match builder.mode(0o700).create(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "invalid sandbox directory",
        ));
    }
    Ok(())
}

fn profile(request: &PreparedRequest) -> Result<String, ()> {
    let candidate = sbpl_path(&request.candidate_root)?;
    let scratch = sbpl_path(&request.scratch_root)?;
    let image = sbpl_path(&request.image_root)?;
    let mut ancestors = request
        .candidate_root
        .ancestors()
        .skip(1)
        .chain(request.scratch_root.ancestors().skip(1))
        .chain(request.image_root.ancestors().skip(1))
        .map(sbpl_path)
        .collect::<Result<Vec<_>, _>>()?;
    ancestors.sort_unstable();
    ancestors.dedup();
    let traversal = ancestors
        .into_iter()
        .map(|path| format!("(allow file-read* (literal \"{path}\"))\n"))
        .collect::<String>();
    Ok(format!(
        "(version 1)\n\
         (deny default)\n\
         (allow process-exec)\n\
         (allow process-fork)\n\
         (allow signal (target same-sandbox))\n\
         (allow process-info* (target same-sandbox))\n\
         (deny process-info-setcontrol)\n\
         (allow sysctl-read)\n\
         (allow mach-lookup (global-name \"com.apple.system.opendirectoryd.libinfo\"))\n\
         (allow file-lock)\n\
         (allow file-read* (subpath \"/System\"))\n\
         (allow file-read* (subpath \"/usr/bin\"))\n\
         (allow file-read* (subpath \"/usr/lib\"))\n\
         (allow file-read* (subpath \"/bin\"))\n\
         (allow file-read* (subpath \"/sbin\"))\n\
         (allow file-read* (subpath \"/Library/Apple\"))\n\
         (allow file-read* (subpath \"/Library/Developer/CommandLineTools\"))\n\
         (allow file-read* (subpath \"/private/var/db/dyld\"))\n\
         (allow file-read* (subpath \"/private/var/select\"))\n\
         (allow file-read* (literal \"/var\"))\n\
         (allow file-read* (literal \"/tmp\"))\n\
         (allow file-read* (literal \"/dev/null\"))\n\
         (allow file-write* (literal \"/dev/null\"))\n\
         (allow file-read* (literal \"/dev/zero\"))\n\
         (allow file-read* (literal \"/dev/random\"))\n\
         (allow file-read* (literal \"/dev/urandom\"))\n\
         (allow file-read* file-write* (subpath \"/dev/fd\"))\n\
         {traversal}\
         (allow file-read* file-write* (subpath \"{candidate}\"))\n\
         (allow process-exec (subpath \"{candidate}\"))\n\
         (allow file-read* file-write* (subpath \"{scratch}\"))\n\
         (allow process-exec (subpath \"{scratch}\"))\n\
         (allow file-read* (subpath \"{image}\"))\n\
         (allow process-exec (subpath \"{image}\"))\n\
         (deny network*)\n"
    ))
}

fn sbpl_path(path: &Path) -> Result<String, ()> {
    let value = path.to_str().ok_or(())?;
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\0' => return Err(()),
            _ => escaped.push(character),
        }
    }
    Ok(escaped)
}

#[cfg(test)]
mod tests {
    use std::{fs::File, io::Write, os::unix::fs::PermissionsExt, path::PathBuf};

    use super::*;
    use crate::{SANDBOX_PROTOCOL_VERSION, SandboxLimits, SandboxRequest};

    struct Fixture {
        parent: PathBuf,
        candidate: PathBuf,
        scratch: PathBuf,
        image: PathBuf,
    }

    impl Fixture {
        fn new(script: &[u8]) -> Self {
            let mut identifier = [0_u8; 16];
            getrandom::fill(&mut identifier).expect("test randomness");
            let parent = std::env::temp_dir().join(format!(
                "morons-seatbelt-test-{}",
                identifier
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            ));
            let candidate = parent.join("candidate");
            let scratch = parent.join("scratch");
            let image = parent.join("image");
            for path in [
                parent.as_path(),
                candidate.as_path(),
                scratch.as_path(),
                image.as_path(),
                image.join("bin").as_path(),
            ] {
                fs::create_dir(path).expect("creates fixture directory");
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                    .expect("sets fixture mode");
            }
            let executable = image.join("bin/fixture");
            let mut file = File::create(&executable).expect("creates fixture executable");
            file.write_all(script).expect("writes fixture executable");
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
                .expect("sets executable mode");
            Self {
                parent,
                candidate,
                scratch,
                image,
            }
        }

        fn request(&self, arguments: Vec<String>) -> SandboxRequest {
            SandboxRequest {
                protocol_version: SANDBOX_PROTOCOL_VERSION,
                operation_id: [9; 16],
                candidate_root: self.candidate.to_string_lossy().into_owned(),
                scratch_root: self.scratch.to_string_lossy().into_owned(),
                image_root: self.image.to_string_lossy().into_owned(),
                executable: "bin/fixture".to_owned(),
                arguments,
                working_directory: ".".to_owned(),
                limits: SandboxLimits {
                    wall_time_milliseconds: 5_000,
                    output_bytes_per_stream: 64 * 1024,
                },
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.parent);
        }
    }

    #[test]
    fn sbpl_paths_escape_profile_metacharacters() {
        assert_eq!(
            sbpl_path(Path::new("/tmp/a\"b\\c")).expect("escapes"),
            r#"/tmp/a\"b\\c"#
        );
        assert!(sbpl_path(Path::new("/tmp/a\0b")).is_err());
    }

    #[test]
    fn seatbelt_command_writes_only_candidate_and_returns_bounded_streams() {
        let fixture = Fixture::new(
            b"#!/bin/sh\nprintf 'hello'\nprintf 'diagnostic' >&2\nprintf 'changed' > created.txt\n",
        );
        let result = crate::execute(fixture.request(Vec::new()), &Cancellation::new());
        assert_eq!(result.status, SandboxStatus::Exited, "{result:?}");
        assert_eq!(
            result.exit.and_then(|exit| exit.code),
            Some(0),
            "exit={:?} stderr={}",
            result.exit,
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(result.stdout, b"hello");
        assert_eq!(result.stderr, b"diagnostic");
        assert!(result.candidate_eligible);
        assert_eq!(
            fs::read(fixture.candidate.join("created.txt")).expect("candidate output"),
            b"changed"
        );
    }

    #[test]
    fn seatbelt_discards_candidates_after_time_and_output_limits() {
        let timeout_fixture = Fixture::new(b"#!/bin/sh\n/bin/sleep 30\n");
        let mut timeout_request = timeout_fixture.request(Vec::new());
        timeout_request.limits.wall_time_milliseconds = 100;
        let timeout = crate::execute(timeout_request, &Cancellation::new());
        assert_eq!(timeout.status, SandboxStatus::TimedOut, "{timeout:?}");
        assert!(!timeout.candidate_eligible);

        let output_fixture = Fixture::new(b"#!/bin/sh\nwhile :; do printf '0123456789'; done\n");
        let mut output_request = output_fixture.request(Vec::new());
        output_request.limits.output_bytes_per_stream = 1_024;
        let output = crate::execute(output_request, &Cancellation::new());
        assert_eq!(output.status, SandboxStatus::OutputLimit, "{output:?}");
        assert!(!output.candidate_eligible);
    }

    #[test]
    fn seatbelt_denies_loopback_and_host_network_access() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("binds listener");
        listener.set_nonblocking(true).expect("sets nonblocking");
        let port = listener.local_addr().expect("listener address").port();
        let fixture = Fixture::new(
            b"#!/bin/sh\n/usr/bin/python3 -c 'import socket,sys; socket.create_connection((\"127.0.0.1\", int(sys.argv[1]))); print(\"connected\")' \"$1\"\n",
        );
        let result = crate::execute(
            fixture.request(vec![port.to_string()]),
            &Cancellation::new(),
        );
        assert_eq!(result.status, SandboxStatus::Exited, "{result:?}");
        assert_ne!(result.exit.and_then(|exit| exit.code), Some(0));
        assert!(
            !result
                .stdout
                .windows(b"connected".len())
                .any(|window| window == b"connected")
        );
        assert_eq!(
            listener
                .accept()
                .expect_err("connection must be denied")
                .kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn seatbelt_denies_process_group_escape() {
        let fixture = Fixture::new(
            b"#!/bin/sh\n/usr/bin/python3 -c 'import os; os.setsid(); print(\"escaped\")'\n",
        );
        let result = crate::execute(fixture.request(Vec::new()), &Cancellation::new());
        assert_eq!(result.status, SandboxStatus::Exited, "{result:?}");
        assert_ne!(result.exit.and_then(|exit| exit.code), Some(0));
        assert!(
            !result
                .stdout
                .windows(b"escaped".len())
                .any(|window| window == b"escaped")
        );
    }

    #[test]
    fn seatbelt_denies_sibling_host_files() {
        let fixture = Fixture::new(b"#!/bin/sh\ncat \"$1\"\n");
        let sentinel = fixture.parent.join("host-sentinel");
        fs::write(&sentinel, b"must-not-be-readable").expect("writes sentinel");
        let result = crate::execute(
            fixture.request(vec![sentinel.to_string_lossy().into_owned()]),
            &Cancellation::new(),
        );
        assert_eq!(result.status, SandboxStatus::Exited, "{result:?}");
        assert_ne!(result.exit.and_then(|exit| exit.code), Some(0));
        assert!(
            !result
                .stdout
                .windows(b"must-not-be-readable".len())
                .any(|window| { window == b"must-not-be-readable" })
        );
    }
}
