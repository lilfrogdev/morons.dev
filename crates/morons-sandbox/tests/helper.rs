#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::{
    fs::{self, File},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use morons_sandbox::{
    SANDBOX_PROTOCOL_VERSION, SandboxLimits, SandboxRequest, SandboxStatus, read_result,
    write_request,
};

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
            "morons-helper-test-{}",
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
            create_private_directory(path);
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

    fn request(&self, wall_time_milliseconds: u64) -> SandboxRequest {
        self.request_with_arguments(wall_time_milliseconds, Vec::new())
    }

    fn request_with_arguments(
        &self,
        wall_time_milliseconds: u64,
        arguments: Vec<String>,
    ) -> SandboxRequest {
        SandboxRequest {
            protocol_version: SANDBOX_PROTOCOL_VERSION,
            operation_id: [11; 16],
            candidate_root: self.candidate.to_string_lossy().into_owned(),
            scratch_root: self.scratch.to_string_lossy().into_owned(),
            image_root: self.image.to_string_lossy().into_owned(),
            executable: "bin/fixture".to_owned(),
            arguments,
            working_directory: ".".to_owned(),
            limits: SandboxLimits {
                wall_time_milliseconds,
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
fn helper_uses_framed_one_shot_control_without_closing_watchdog() {
    let fixture = Fixture::new(b"#!/bin/sh\nprintf 'from-helper'\n");
    let mut child = helper();
    let mut input = child.stdin.take().expect("helper stdin");
    write_request(&mut input, &fixture.request(5_000)).expect("writes request");
    let result =
        read_result(&mut child.stdout.take().expect("helper stdout")).expect("reads result");
    assert_eq!(result.status, SandboxStatus::Exited, "{result:?}");
    assert_eq!(result.stdout, b"from-helper");
    assert!(result.candidate_eligible);
    drop(input);
    assert!(child.wait().expect("waits helper").success());
}

#[test]
fn helper_watchdog_cancels_when_the_server_channel_closes() {
    let fixture = Fixture::new(b"#!/bin/sh\n/bin/sleep 30\n");
    let mut child = helper();
    let mut input = child.stdin.take().expect("helper stdin");
    write_request(&mut input, &fixture.request(60_000)).expect("writes request");
    drop(input);
    let result =
        read_result(&mut child.stdout.take().expect("helper stdout")).expect("reads result");
    assert_eq!(result.status, SandboxStatus::Cancelled, "{result:?}");
    assert!(!result.candidate_eligible);
    assert!(child.wait().expect("waits helper").success());
}

#[cfg(target_os = "linux")]
#[test]
fn linux_helper_confines_files_network_processes_and_namespaces() {
    use std::net::TcpListener;

    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("binds listener");
    listener.set_nonblocking(true).expect("sets nonblocking");
    let port = listener.local_addr().expect("listener address").port();
    let fixture = Fixture::new(
        b"#!/bin/sh\n\
          set -eu\n\
          printf 'changed' > created.txt\n\
          if /usr/bin/cat \"$1\" >/tmp/leak 2>/dev/null; then exit 71; fi\n\
          if /usr/bin/bash -c \"echo connected >/dev/tcp/127.0.0.1/$2\" 2>/dev/null; then exit 72; fi\n\
          if /usr/bin/unshare --user /usr/bin/true 2>/dev/null; then exit 73; fi\n\
          if /usr/bin/setsid /usr/bin/true 2>/dev/null; then exit 74; fi\n\
          if /usr/bin/kill -0 \"$3\" 2>/dev/null; then exit 75; fi\n\
          if printf 'tamper' > /image/tamper 2>/dev/null; then exit 76; fi\n\
          if /usr/bin/cat /proc/self/mountinfo >/tmp/mounts 2>/dev/null; then exit 77; fi\n\
          test \"$(/usr/bin/hostname)\" = morons-sandbox\n\
          printf 'confined'\n",
    );
    let sentinel = fixture.parent.join("host-sentinel");
    fs::write(&sentinel, b"must-not-be-readable").expect("writes sentinel");
    let result = invoke_helper(fixture.request_with_arguments(
        5_000,
        vec![
            sentinel.to_string_lossy().into_owned(),
            port.to_string(),
            std::process::id().to_string(),
        ],
    ));
    assert_eq!(result.status, SandboxStatus::Exited, "{result:?}");
    assert_eq!(
        result.exit.and_then(|exit| exit.code),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"confined");
    assert!(result.candidate_eligible);
    assert_eq!(
        fs::read(fixture.candidate.join("created.txt")).expect("candidate output"),
        b"changed"
    );
    assert!(!fixture.image.join("tamper").exists());
    assert_eq!(
        listener
            .accept()
            .expect_err("connection must be denied")
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_helper_stops_background_descendants_and_forced_terminations() {
    use std::time::{Duration, Instant};

    let background = Fixture::new(b"#!/bin/sh\n/usr/bin/sleep 30 &\nprintf 'done'\n");
    let started = Instant::now();
    let result = invoke_helper(background.request(5_000));
    assert_eq!(result.status, SandboxStatus::Exited, "{result:?}");
    assert_eq!(result.exit.and_then(|exit| exit.code), Some(0));
    assert_eq!(result.stdout, b"done");
    assert!(started.elapsed() < Duration::from_secs(3));

    let signalled = Fixture::new(b"#!/bin/sh\nprintf 'before-signal'\n/bin/kill -TERM $$\n");
    let result = invoke_helper(signalled.request(5_000));
    assert_eq!(result.status, SandboxStatus::Signalled, "{result:?}");
    assert_eq!(result.exit.and_then(|exit| exit.signal), Some(15));
    assert_eq!(result.stdout, b"before-signal");
    assert!(!result.candidate_eligible);

    let timeout = Fixture::new(b"#!/bin/sh\n/usr/bin/sleep 30\n");
    let result = invoke_helper(timeout.request(100));
    assert_eq!(result.status, SandboxStatus::TimedOut, "{result:?}");
    assert!(!result.candidate_eligible);

    let output = Fixture::new(b"#!/bin/sh\nwhile :; do printf '0123456789'; done\n");
    let mut request = output.request(5_000);
    request.limits.output_bytes_per_stream = 1_024;
    let result = invoke_helper(request);
    assert_eq!(result.status, SandboxStatus::OutputLimit, "{result:?}");
    assert!(!result.candidate_eligible);

    let processes = Fixture::new(
        b"#!/bin/sh\ni=0\nwhile [ \"$i\" -lt 100 ]; do /usr/bin/sleep 30 & i=$((i + 1)); done\nwait\n",
    );
    let result = invoke_helper(processes.request(5_000));
    assert_eq!(result.status, SandboxStatus::ResourceLimit, "{result:?}");
    assert!(!result.candidate_eligible);
}

#[cfg(target_os = "linux")]
#[test]
fn linux_helper_loss_terminates_the_complete_namespace() {
    use std::{thread, time::Duration};

    let fixture = Fixture::new(
        b"#!/bin/sh\nprintf 'started' > started\n/usr/bin/sleep 1\nprintf 'escaped' > survived\n",
    );
    let mut child = helper();
    let mut input = child.stdin.take().expect("helper stdin");
    write_request(&mut input, &fixture.request(5_000)).expect("writes request");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !fixture.candidate.join("started").exists() && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(fixture.candidate.join("started").exists());
    child.kill().expect("kills helper");
    child.wait().expect("reaps helper");
    drop(input);
    thread::sleep(Duration::from_millis(1_200));
    assert!(!fixture.candidate.join("survived").exists());
}

#[cfg(target_os = "linux")]
fn invoke_helper(request: SandboxRequest) -> morons_sandbox::SandboxResult {
    let mut child = helper();
    let mut input = child.stdin.take().expect("helper stdin");
    write_request(&mut input, &request).expect("writes request");
    let result = read_result(&mut child.stdout.take().expect("helper stdout"))
        .expect("reads sandbox result");
    drop(input);
    assert!(child.wait().expect("waits helper").success());
    result
}

fn helper() -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_morons-sandbox"))
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("launches helper")
}

fn create_private_directory(path: &Path) {
    fs::create_dir(path).expect("creates fixture directory");
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).expect("sets fixture mode");
}
