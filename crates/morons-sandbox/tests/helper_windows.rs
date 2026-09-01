#![cfg(target_os = "windows")]

use std::{
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
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
    fn new() -> Self {
        let mut identifier = [0_u8; 16];
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
        let command = std::env::var_os("ComSpec").expect("ComSpec");
        fs::copy(command, image.join("bin/fixture.exe")).expect("copies command fixture");
        Self {
            parent,
            candidate,
            scratch,
            image,
        }
    }

    fn request(&self, command: String, wall_time_milliseconds: u64) -> SandboxRequest {
        SandboxRequest {
            protocol_version: SANDBOX_PROTOCOL_VERSION,
            operation_id: [23; 16],
            candidate_root: utf8(&self.candidate),
            scratch_root: utf8(&self.scratch),
            image_root: utf8(&self.image),
            executable: "bin/fixture.exe".to_owned(),
            arguments: vec!["/D".to_owned(), "/S".to_owned(), "/C".to_owned(), command],
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
    let fixture = Fixture::new();
    let sentinel = fixture.parent.join("host-sentinel");
    fs::write(&sentinel, b"must-not-be-readable").expect("writes sentinel");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("binds listener");
    listener.set_nonblocking(true).expect("sets nonblocking");
    let port = listener.local_addr().expect("listener address").port();
    let command = format!(
        "echo changed>created.txt & \
         type \"{}\" >nul 2>&1 && exit /b 71 || ver>nul & \
         if defined MORONS_SECRET_SENTINEL exit /b 72 & \
         echo tamper>\"%PATH%\\..\\tamper\" 2>nul && exit /b 73 || ver>nul & \
         %SystemRoot%\\System32\\WindowsPowerShell\\v1.0\\powershell.exe -NoProfile -NonInteractive -Command \
         \"$c=New-Object Net.Sockets.TcpClient; try {{$c.Connect('127.0.0.1',{}); exit 74}} catch {{exit 0}}\" & \
         if errorlevel 1 exit /b 75 & echo confined",
        sentinel.display(),
        port
    );
    let result = invoke_helper(fixture.request(command, 10_000));
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
        b"changed\r\n"
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
    let timeout = Fixture::new();
    let result = invoke_helper(timeout.request(
        "%SystemRoot%\\System32\\WindowsPowerShell\\v1.0\\powershell.exe -NoProfile -NonInteractive -Command \"Start-Sleep -Seconds 30\"".to_owned(),
        100,
    ));
    assert_eq!(result.status, SandboxStatus::TimedOut, "{result:?}");
    assert!(!result.candidate_eligible);

    let output = Fixture::new();
    let mut request = output.request(
        "for /L %i in (1,1,100000) do @echo 0123456789".to_owned(),
        10_000,
    );
    request.limits.output_bytes_per_stream = 1_024;
    let result = invoke_helper(request);
    assert_eq!(result.status, SandboxStatus::OutputLimit, "{result:?}");
    assert!(!result.candidate_eligible);

    let background = Fixture::new();
    let command = "start \"\" /B %SystemRoot%\\System32\\WindowsPowerShell\\v1.0\\powershell.exe -NoProfile -NonInteractive -Command \
                   \"Start-Sleep -Seconds 10; [IO.File]::WriteAllText('survived','escaped')\""
        .to_owned();
    let result = invoke_helper(background.request(command, 100));
    assert_eq!(result.status, SandboxStatus::TimedOut, "{result:?}");
    thread::sleep(Duration::from_millis(500));
    assert!(!background.candidate.join("survived").exists());
}

#[test]
fn appcontainer_job_closes_when_the_helper_is_lost() {
    let fixture = Fixture::new();
    let command = "echo started>started & %SystemRoot%\\System32\\WindowsPowerShell\\v1.0\\powershell.exe -NoProfile -NonInteractive -Command \
                   \"Start-Sleep -Seconds 10\" & echo escaped>survived"
        .to_owned();
    let mut child = helper();
    let mut input = child.stdin.take().expect("helper stdin");
    write_request(&mut input, &fixture.request(command, 30_000)).expect("writes request");
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
        .envs(required_windows_environment())
        .env("MORONS_SECRET_SENTINEL", "must-not-cross")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("launches helper")
}

fn required_windows_environment() -> Vec<(String, String)> {
    [
        "SystemRoot",
        "windir",
        "SystemDrive",
        "ComSpec",
        "PATHEXT",
        "PATH",
        "OS",
        "PROCESSOR_ARCHITECTURE",
        "PROCESSOR_IDENTIFIER",
        "PROCESSOR_LEVEL",
        "PROCESSOR_REVISION",
        "NUMBER_OF_PROCESSORS",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
        "ALLUSERSPROFILE",
    ]
    .into_iter()
    .map(|name| {
        (
            name.to_owned(),
            std::env::var(name).expect("required Windows environment"),
        )
    })
    .collect()
}

fn utf8(path: &Path) -> String {
    path.to_str().expect("UTF-8 test path").to_owned()
}
