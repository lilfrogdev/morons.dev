#![cfg(target_os = "macos")]

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
        SandboxRequest {
            protocol_version: SANDBOX_PROTOCOL_VERSION,
            operation_id: [11; 16],
            candidate_root: self.candidate.to_string_lossy().into_owned(),
            scratch_root: self.scratch.to_string_lossy().into_owned(),
            image_root: self.image.to_string_lossy().into_owned(),
            executable: "bin/fixture".to_owned(),
            arguments: Vec::new(),
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
    let fixture = Fixture::new(b"#!/bin/sh\nsleep 30\n");
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
