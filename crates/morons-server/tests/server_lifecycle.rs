#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::{DirBuilderExt as _, PermissionsExt as _},
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use morons_cli::{ApplicationClient, ConnectOrStartError, connect_or_start, perform_handshake};
use morons_protocol::{
    ClientEndpoint, ClientEndpointDiscovery, ControlError, MutationRequestId,
    SessionCatalogEventCursor, authenticate_client,
};

const HELPER_ENVIRONMENT: &str = "MORONS_SERVER_LIFECYCLE_HELPER";
const AUTO_START_HELPER_ENVIRONMENT: &str = "MORONS_AUTO_START_HELPER";
const INCOMPLETE_CONTROL_HELPER_ENVIRONMENT: &str = "MORONS_INCOMPLETE_CONTROL_HELPER";
const CONCURRENT_CONNECT_HELPER_ENVIRONMENT: &str = "MORONS_CONCURRENT_CONNECT_HELPER";
const STOP_EXISTING_HELPER_ENVIRONMENT: &str = "MORONS_STOP_EXISTING_HELPER";
const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);
const RETRY_DELAY: Duration = Duration::from_millis(25);
static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static PROCESS_TEST_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn authenticated_server_stop_exits_and_removes_current_registration() {
    let _process_test = process_test_guard();
    let home = test_home();
    let mut server = minimal_command(PathBuf::from(env!("CARGO_BIN_EXE_morons-server")), &home)
        .spawn()
        .expect("server process should start");
    let mut helper = minimal_command(
        std::env::current_exe().expect("test executable path should be available"),
        &home,
    );
    helper
        .arg("--exact")
        .arg("authenticated_stop_helper")
        .arg("--nocapture")
        .env(HELPER_ENVIRONMENT, "1");
    let mut helper = helper.spawn().expect("lifecycle helper should start");

    let helper_status = wait_for_exit(&mut helper, PROCESS_TIMEOUT);
    if !helper_status.as_ref().is_ok_and(ExitStatus::success) {
        let _ = server.kill();
        let _ = server.wait();
        panic!("authenticated lifecycle helper failed: {helper_status:?}");
    }
    let server_status = wait_for_exit(&mut server, PROCESS_TIMEOUT)
        .expect("server should exit after authenticated stop");
    assert!(server_status.success());
    assert!(!home.join(".morons/control/endpoint.json").exists());
    fs::remove_dir_all(home).expect("test home should be removable");
}

#[test]
fn exact_sibling_companion_is_started_and_authenticated() {
    let _process_test = process_test_guard();
    let home = test_private_directory("auto-start-home");
    let (package, client) = package_test_client("auto-start-package", true);

    let mut helper = minimal_command(client, &home);
    helper
        .arg("--exact")
        .arg("auto_start_helper")
        .arg("--nocapture")
        .env(AUTO_START_HELPER_ENVIRONMENT, "1");
    let mut helper = helper.spawn().expect("auto-start helper should start");
    let status = wait_for_exit(&mut helper, PROCESS_TIMEOUT)
        .expect("auto-start helper should exit within its deadline");
    assert!(status.success());
    assert!(!home.join(".morons/control/endpoint.json").exists());
    fs::remove_dir_all(home).expect("test home should be removable");
    fs::remove_dir_all(package).expect("test package should be removable");
}

#[test]
fn incomplete_control_state_fails_without_launching_a_replacement() {
    let _process_test = process_test_guard();
    let home = test_private_directory("incomplete-home");
    let application_root = home.join(".morons");
    let control_root = application_root.join("control");
    fs::create_dir(&application_root).expect("test application root should be created");
    fs::create_dir(&control_root).expect("test control root should be created");
    fs::set_permissions(&application_root, fs::Permissions::from_mode(0o700))
        .expect("test application root should be owner-only");
    fs::set_permissions(&control_root, fs::Permissions::from_mode(0o700))
        .expect("test control root should be owner-only");

    let (package, client) = package_test_client("incomplete-package", false);
    let mut helper = minimal_command(client, &home);
    helper
        .arg("--exact")
        .arg("incomplete_control_helper")
        .arg("--nocapture")
        .env(INCOMPLETE_CONTROL_HELPER_ENVIRONMENT, "1");
    let mut helper = helper
        .spawn()
        .expect("incomplete-control helper should start");
    let status = wait_for_exit(&mut helper, Duration::from_secs(5))
        .expect("incomplete-control helper should fail closed promptly");
    assert!(status.success());
    assert!(!package.join("morons-server").exists());
    assert!(!control_root.join("endpoint.json").exists());
    fs::remove_dir_all(home).expect("test home should be removable");
    fs::remove_dir_all(package).expect("test package should be removable");
}

#[test]
fn concurrent_clients_resolve_one_authenticated_server() {
    let _process_test = process_test_guard();
    let home = test_private_directory("concurrent-home");
    let (package, client) = package_test_client("concurrent-package", true);
    let mut first = minimal_command(client.clone(), &home);
    first
        .arg("--exact")
        .arg("concurrent_connect_helper")
        .arg("--nocapture")
        .env(CONCURRENT_CONNECT_HELPER_ENVIRONMENT, "1");
    let mut second = minimal_command(client.clone(), &home);
    second
        .arg("--exact")
        .arg("concurrent_connect_helper")
        .arg("--nocapture")
        .env(CONCURRENT_CONNECT_HELPER_ENVIRONMENT, "1");
    let mut first = first.spawn().expect("first concurrent client should start");
    let mut second = second
        .spawn()
        .expect("second concurrent client should start");
    assert!(
        wait_for_exit(&mut first, PROCESS_TIMEOUT)
            .expect("first concurrent client should exit")
            .success()
    );
    assert!(
        wait_for_exit(&mut second, PROCESS_TIMEOUT)
            .expect("second concurrent client should exit")
            .success()
    );
    fs::remove_file(package.join("morons-server"))
        .expect("running companion artifact should be removable");

    let mut stop = minimal_command(client, &home);
    stop.arg("--exact")
        .arg("stop_existing_helper")
        .arg("--nocapture")
        .env(STOP_EXISTING_HELPER_ENVIRONMENT, "1");
    let mut stop = stop.spawn().expect("stop helper should start");
    assert!(
        wait_for_exit(&mut stop, PROCESS_TIMEOUT)
            .expect("stop helper should exit")
            .success()
    );
    assert!(!home.join(".morons/control/endpoint.json").exists());
    fs::remove_dir_all(home).expect("test home should be removable");
    fs::remove_dir_all(package).expect("test package should be removable");
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_connect_helper() {
    if std::env::var_os(CONCURRENT_CONNECT_HELPER_ENVIRONMENT).is_none() {
        return;
    }
    let connected = connect_or_start()
        .await
        .expect("concurrent client should authenticate one server");
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(connected);
}

#[tokio::test(flavor = "current_thread")]
async fn stop_existing_helper() {
    if std::env::var_os(STOP_EXISTING_HELPER_ENVIRONMENT).is_none() {
        return;
    }
    let connected = connect_or_start()
        .await
        .expect("existing server should authenticate");
    assert!(!connected.launched_companion());
    let mut client = ApplicationClient::from_negotiated_connection(connected.into_connection());
    let accepted = client
        .stop_server(MutationRequestId::from_bytes([0x63; 16]))
        .await
        .expect("existing server should accept stop");
    assert!(accepted.current_server_stopping);
    wait_for_absent_registration().await;
}

#[tokio::test(flavor = "current_thread")]
async fn incomplete_control_helper() {
    if std::env::var_os(INCOMPLETE_CONTROL_HELPER_ENVIRONMENT).is_none() {
        return;
    }
    assert!(matches!(
        connect_or_start().await,
        Err(ConnectOrStartError::Control(
            ControlError::InvalidState { .. }
        ))
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn auto_start_helper() {
    if std::env::var_os(AUTO_START_HELPER_ENVIRONMENT).is_none() {
        return;
    }

    let connected = connect_or_start()
        .await
        .expect("exact sibling server should start and authenticate");
    assert!(connected.launched_companion());
    let home = PathBuf::from(std::env::var_os("HOME").expect("test HOME should be set"));
    let registration = fs::read(home.join(".morons/control/endpoint.json"))
        .expect("server registration should be readable");
    let registration: serde_json::Value =
        serde_json::from_slice(&registration).expect("server registration should decode");
    let process_id = registration["server_process_id"]
        .as_u64()
        .and_then(|process_id| i32::try_from(process_id).ok())
        .and_then(rustix::process::Pid::from_raw)
        .expect("registered server process should be valid");
    assert_eq!(
        rustix::process::getpgid(Some(process_id))
            .expect("server process group should be readable"),
        process_id
    );
    assert_ne!(process_id, rustix::process::getpgrp());
    let mut client = ApplicationClient::from_negotiated_connection(connected.into_connection());
    let accepted = client
        .stop_server(MutationRequestId::from_bytes([0x62; 16]))
        .await
        .expect("auto-started server should accept stop");
    assert!(accepted.current_server_stopping);
    wait_for_absent_registration().await;
}

#[tokio::test(flavor = "current_thread")]
async fn authenticated_stop_helper() {
    if std::env::var_os(HELPER_ENVIRONMENT).is_none() {
        return;
    }

    let deadline = tokio::time::Instant::now() + PROCESS_TIMEOUT;
    let endpoint = loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "server registration should become available"
        );
        match ClientEndpoint::discover().expect("control state should remain valid") {
            ClientEndpointDiscovery::Registered(endpoint) => break endpoint,
            ClientEndpointDiscovery::Absent
            | ClientEndpointDiscovery::Incomplete
            | ClientEndpointDiscovery::Starting => {
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    };
    let subscription_connection = authenticated_connection(&endpoint, deadline).await;
    let subscription_client =
        ApplicationClient::from_negotiated_connection(subscription_connection);
    let mut subscription = subscription_client
        .subscribe_to_session_catalog(SessionCatalogEventCursor::beginning())
        .await
        .expect("catalog subscription should start");
    let connection = authenticated_connection(&endpoint, deadline).await;
    let mut client = ApplicationClient::from_negotiated_connection(connection);
    let accepted = client
        .stop_server(MutationRequestId::from_bytes([0x61; 16]))
        .await
        .expect("authenticated stop should be accepted");
    assert!(accepted.current_server_stopping);
    drop(client);
    assert!(
        tokio::time::timeout(Duration::from_secs(3), subscription.next_event())
            .await
            .expect("subscription should close within the server deadline")
            .is_err()
    );

    wait_for_absent_registration().await;
}

async fn authenticated_connection(
    endpoint: &ClientEndpoint,
    deadline: tokio::time::Instant,
) -> interprocess::local_socket::tokio::Stream {
    let mut connection = tokio::time::timeout_at(deadline, endpoint.connect())
        .await
        .expect("server connection should not time out")
        .expect("server connection should succeed");
    endpoint
        .verify_connected_server(&connection)
        .expect("server peer should be authorized");
    tokio::time::timeout_at(
        deadline,
        authenticate_client(
            &mut connection,
            endpoint.authentication_key(),
            endpoint.host_epoch(),
        ),
    )
    .await
    .expect("server authentication should not time out")
    .expect("server should authenticate");
    tokio::time::timeout_at(
        deadline,
        perform_handshake(&mut connection, "lifecycle-test"),
    )
    .await
    .expect("server handshake should not time out")
    .expect("server handshake should succeed");
    connection
}

async fn wait_for_absent_registration() {
    let deadline = tokio::time::Instant::now() + PROCESS_TIMEOUT;
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "server registration should be removed"
        );
        match ClientEndpoint::discover().expect("control state should remain valid") {
            ClientEndpointDiscovery::Absent => break,
            ClientEndpointDiscovery::Incomplete
            | ClientEndpointDiscovery::Starting
            | ClientEndpointDiscovery::Registered(_) => {
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    }
}

fn package_test_client(label: &str, include_server: bool) -> (PathBuf, PathBuf) {
    let package = test_private_directory(label);
    let client = package.join("morons");
    fs::copy(
        std::env::current_exe().expect("test executable path should be available"),
        &client,
    )
    .expect("test client should be packaged");
    fs::set_permissions(&client, fs::Permissions::from_mode(0o700))
        .expect("test client should be executable");
    if include_server {
        let server = package.join("morons-server");
        fs::copy(env!("CARGO_BIN_EXE_morons-server"), &server)
            .expect("test server should be packaged");
        fs::set_permissions(&server, fs::Permissions::from_mode(0o700))
            .expect("test server should be executable");
    }
    (package, client)
}

fn minimal_command(program: PathBuf, home: &PathBuf) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("HOME", home)
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> Result<ExitStatus, &'static str> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().map_err(|_| "process wait failed")? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("process exit timed out");
        }
        thread::sleep(RETRY_DELAY);
    }
}

fn process_test_guard() -> std::sync::MutexGuard<'static, ()> {
    PROCESS_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn test_home() -> PathBuf {
    test_private_directory("lifecycle-home")
}

fn test_private_directory(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("test time should be available")
        .as_nanos();
    let sequence = TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nonce = nonce as u64;
    let path = PathBuf::from("/tmp").join(format!(
        "mz-{:x}-{nonce:x}-{sequence:x}",
        std::process::id()
    ));
    let _ = label;
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(&path).expect("test home should be created");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
        .expect("test home should be owner-only");
    path
}
