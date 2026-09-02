#![cfg(target_os = "windows")]

use std::{
    fs,
    io::Write,
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};

use morons_sandbox::{
    SANDBOX_PROTOCOL_VERSION, SandboxLimits, SandboxRequest, SandboxStatus, read_result,
    write_request,
};

static SANDBOX_TEST_LOCK: Mutex<()> = Mutex::new(());

struct Fixture {
    operation_id: [u8; 16],
    parent: PathBuf,
    candidate: PathBuf,
    scratch: PathBuf,
    image: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let mut identifier = [0u8; 16];
        getrandom::fill(&mut identifier).expect("test randomness");
        let parent = std::env::temp_dir().join(format!(
            "morons-windows-helper-test-{}",
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
        }
        fs::copy(
            std::env::current_exe().expect("integration test executable"),
            image.join("bin/fixture.exe"),
        )
        .expect("copies command fixture");
        Self {
            operation_id: identifier,
            parent,
            candidate,
            scratch,
            image,
        }
    }

    fn request(&self, child_test: &str, wall_time_milliseconds: u64) -> SandboxRequest {
        SandboxRequest {
            protocol_version: SANDBOX_PROTOCOL_VERSION,
            operation_id: self.operation_id,
            candidate_root: utf8(&self.candidate),
            scratch_root: utf8(&self.scratch),
            image_root: utf8(&self.image),
            executable: "bin/fixture.exe".to_owned(),
            arguments: vec![
                "--exact".to_owned(),
                child_test.to_owned(),
                "--nocapture".to_owned(),
            ],
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
fn appcontainer_confines_files_environment_and_network() {
    let _guard = sandbox_test_guard();
    let fixture = Fixture::new();
    fs::write(
        fixture.parent.join("host-sentinel"),
        b"must-not-be-readable",
    )
    .expect("writes sentinel");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("binds listener");
    listener.set_nonblocking(true).expect("sets nonblocking");
    let port = listener.local_addr().expect("listener address").port();
    fs::write(fixture.candidate.join("network-port"), port.to_string())
        .expect("writes network fixture");

    let result = invoke_helper(fixture.request("sandbox_child_confine", 10_000));
    assert_eq!(result.status, SandboxStatus::Exited, "{result:?}");
    assert_eq!(result.exit.and_then(|exit| exit.code), Some(0));
    assert!(
        String::from_utf8_lossy(&result.stdout).contains("confined"),
        "stdout={}",
        String::from_utf8_lossy(&result.stdout)
    );
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

#[test]
fn appcontainer_enforces_timeout_output_and_background_tree_ownership() {
    let _guard = sandbox_test_guard();
    let timeout = Fixture::new();
    let result = invoke_helper(timeout.request("sandbox_child_timeout", 100));
    assert_eq!(result.status, SandboxStatus::TimedOut, "{result:?}");
    assert!(!result.candidate_eligible);

    let output = Fixture::new();
    let mut request = output.request("sandbox_child_output", 10_000);
    request.limits.output_bytes_per_stream = 1_024;
    let result = invoke_helper(request);
    assert_eq!(result.status, SandboxStatus::OutputLimit, "{result:?}");
    assert!(!result.candidate_eligible);

    let background = Fixture::new();
    let result = invoke_helper(background.request("sandbox_child_background", 10_000));
    let descendants_denied = background.candidate.join("descendants-denied").exists();
    if descendants_denied {
        assert_eq!(result.status, SandboxStatus::Exited, "{result:?}");
        assert_eq!(result.exit.and_then(|exit| exit.code), Some(0));
    } else {
        assert_eq!(
            result.status,
            SandboxStatus::ResourceLimit,
            "{result:?}, stdout={}, stderr={}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
    thread::sleep(Duration::from_millis(500));
    assert!(!background.candidate.join("survived").exists());
}

#[test]
fn appcontainer_job_closes_when_the_helper_is_lost() {
    let _guard = sandbox_test_guard();
    let fixture = Fixture::new();
    let mut child = helper();
    let mut input = child.stdin.take().expect("helper stdin");
    write_request(
        &mut input,
        &fixture.request("sandbox_child_helper_loss", 30_000),
    )
    .expect("writes request");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !fixture.candidate.join("started").exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(fixture.candidate.join("started").exists());
    child.kill().expect("kills helper");
    child.wait().expect("reaps helper");
    drop(input);
    thread::sleep(Duration::from_millis(500));
    assert!(!fixture.candidate.join("survived").exists());
}

#[test]
fn sandbox_child_confine() {
    if !inside_sandbox() {
        return;
    }
    fs::write("created.txt", b"changed").expect("candidate should be writable");
    assert!(fs::read("../host-sentinel").is_err());
    assert!(std::env::var_os("MORONS_SECRET_SENTINEL").is_none());
    let image_bin = PathBuf::from(std::env::var_os("PATH").expect("fixed image PATH"));
    assert!(fs::write(image_bin.join("..").join("tamper"), b"tamper").is_err());
    let port = fs::read_to_string("network-port")
        .expect("network fixture")
        .parse::<u16>()
        .expect("network port");
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    assert!(TcpStream::connect_timeout(&address, Duration::from_millis(500)).is_err());
    println!("confined");
}

#[test]
fn sandbox_child_timeout() {
    if inside_sandbox() {
        thread::sleep(Duration::from_secs(30));
    }
}

#[test]
fn sandbox_child_output() {
    if !inside_sandbox() {
        return;
    }
    let mut stdout = std::io::stdout().lock();
    for _ in 0..100_000 {
        stdout
            .write_all(b"0123456789\n")
            .expect("sandbox output should initially drain");
    }
}

#[test]
fn sandbox_child_background() {
    if !inside_sandbox() {
        return;
    }
    let spawned = Command::new(std::env::current_exe().expect("private executable"))
        .args(["--exact", "sandbox_child_descendant", "--nocapture"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(mut child) => {
            thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            fs::write("descendants-denied", b"host-policy")
                .expect("candidate should record restrictive host policy");
        }
        Err(error) => panic!("background descendant should start or be denied: {error}"),
    }
}

#[test]
fn sandbox_child_descendant() {
    if !inside_sandbox() {
        return;
    }
    thread::sleep(Duration::from_secs(10));
    fs::write("survived", b"escaped").expect("candidate remains writable");
}

#[test]
fn sandbox_child_helper_loss() {
    if !inside_sandbox() {
        return;
    }
    fs::write("started", b"started").expect("candidate should be writable");
    thread::sleep(Duration::from_secs(10));
    fs::write("survived", b"escaped").expect("candidate remains writable");
}

fn sandbox_test_guard() -> MutexGuard<'static, ()> {
    SANDBOX_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn inside_sandbox() -> bool {
    std::env::var_os("MORONS_SANDBOX").as_deref() == Some(std::ffi::OsStr::new("1"))
}

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
        .env("MORONS_SECRET_SENTINEL", "must-not-cross")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("launches helper")
}

fn utf8(path: &Path) -> String {
    path.to_str().expect("UTF-8 test path").to_owned()
}
