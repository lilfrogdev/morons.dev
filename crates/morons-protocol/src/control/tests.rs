use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use super::load_authentication_key;
use super::{ClientEndpoint, ControlError, ControlPaths, ServerEndpoint};

#[tokio::test(flavor = "current_thread")]
async fn duplicate_server_is_rejected_by_lifetime_lock() {
    let paths = temporary_control_paths("duplicate-lock");
    let server =
        ServerEndpoint::bind_with_paths(paths.clone()).expect("first server endpoint should bind");

    let error = match ServerEndpoint::bind_with_paths(paths.clone()) {
        Ok(_) => panic!("second server endpoint should be rejected"),
        Err(error) => error,
    };
    assert!(matches!(error, ControlError::HostAlreadyRunning));

    drop(server);
    std::fs::remove_dir_all(paths.root_directory).expect("test control root should be removable");
}

#[tokio::test(flavor = "current_thread")]
async fn successor_recovers_constrained_stale_registration() {
    let paths = temporary_control_paths("stale-registration");
    let server =
        ServerEndpoint::bind_with_paths(paths.clone()).expect("first server endpoint should bind");
    let ServerEndpoint {
        listener,
        mut control,
    } = server;
    drop(listener);
    control.published = false;
    drop(control);
    assert!(paths.registration_path().exists());

    let successor = ServerEndpoint::bind_with_paths(paths.clone())
        .expect("successor should recover stale registration");
    let client = ClientEndpoint::load_with_paths(paths.clone())
        .expect("client should load successor registration");
    assert_eq!(client.host_epoch(), successor.host_epoch());

    drop(client);
    drop(successor);
    std::fs::remove_dir_all(paths.root_directory).expect("test control root should be removable");
}

#[test]
fn endpoint_registration_rejects_unknown_fields() {
    let registration = br#"{
        "registration_schema_version": 1,
        "authentication_protocol_version": 1,
        "host_epoch": "00000000000000000000000000000000",
        "endpoint": "server-00000000000000000000000000000000.sock",
        "server_process_id": 1,
        "unexpected": true
    }"#;

    assert!(serde_json::from_slice::<super::EndpointRegistration>(registration).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn client_loads_published_endpoint_and_authenticates() {
    let paths = temporary_control_paths("published-endpoint");
    let server =
        ServerEndpoint::bind_with_paths(paths.clone()).expect("server endpoint should bind");
    let client =
        ClientEndpoint::load_with_paths(paths.clone()).expect("client endpoint should load");

    let exchange = async {
        let accepted = server.accept();
        let connected = client.connect();
        let (server_connection, client_connection) = tokio::join!(accepted, connected);
        (
            server_connection.expect("server should accept client"),
            client_connection.expect("client should connect"),
        )
    };
    let (mut server_connection, mut client_connection) = exchange.await;

    client
        .verify_connected_server(&client_connection)
        .expect("client should verify connected server");
    crate::endpoint::authorize_accepted_peer(&server_connection)
        .expect("server should authorize accepted client");

    let client_authentication = crate::authenticate_client(
        &mut client_connection,
        client.authentication_key(),
        client.host_epoch(),
    );
    let server_authentication = crate::authenticate_server(
        &mut server_connection,
        server.authentication_key(),
        server.host_epoch(),
    );
    let (client_result, server_result) = tokio::join!(client_authentication, server_authentication);
    client_result.expect("client authentication should succeed");
    server_result.expect("server authentication should succeed");

    drop(client_connection);
    drop(server_connection);
    drop(client);
    drop(server);
    std::fs::remove_dir_all(paths.root_directory).expect("test control root should be removable");
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn startup_removes_owner_controlled_orphan_from_prepublication_crash() {
    let paths = temporary_control_paths("orphan-endpoint");
    let server =
        ServerEndpoint::bind_with_paths(paths.clone()).expect("server endpoint should bind");
    drop(server);

    let orphan_path = paths
        .runtime_directory
        .join("server-11111111111111111111111111111111.sock");
    let orphan = std::os::unix::net::UnixListener::bind(&orphan_path)
        .expect("orphan test socket should bind");
    drop(orphan);
    assert!(orphan_path.exists());

    let successor = ServerEndpoint::bind_with_paths(paths.clone())
        .expect("successor should remove orphan endpoint");
    assert!(!orphan_path.exists());

    drop(successor);
    std::fs::remove_dir_all(paths.root_directory).expect("test control root should be removable");
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn insecure_existing_authentication_key_fails_closed() {
    use std::os::unix::fs::PermissionsExt;

    let paths = temporary_control_paths("insecure-key");
    let server =
        ServerEndpoint::bind_with_paths(paths.clone()).expect("server endpoint should bind");
    drop(server);

    let key_path = paths.authentication_key_path();
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644))
        .expect("test should make authentication key insecure");
    let error = match ServerEndpoint::bind_with_paths(paths.clone()) {
        Ok(_) => panic!("server should reject insecure authentication key"),
        Err(error) => error,
    };
    assert!(matches!(error, ControlError::InvalidState { .. }));

    std::fs::remove_dir_all(paths.root_directory).expect("test control root should be removable");
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn authentication_key_is_stored_with_owner_only_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let paths = temporary_control_paths("key-permissions");
    let server =
        ServerEndpoint::bind_with_paths(paths.clone()).expect("server endpoint should bind");
    let key_path = paths.authentication_key_path();

    let mode = std::fs::symlink_metadata(&key_path)
        .expect("authentication key metadata should be available")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    load_authentication_key(&key_path).expect("authentication key should load");

    drop(server);
    std::fs::remove_dir_all(paths.root_directory).expect("test control root should be removable");
}

fn temporary_control_paths(label: &str) -> ControlPaths {
    static NEXT_PATH_ID: AtomicU64 = AtomicU64::new(0);

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after the Unix epoch")
        .as_nanos();
    let path_id = NEXT_PATH_ID.fetch_add(1, Ordering::Relaxed);
    temporary_control_paths_from_parts(label, std::process::id(), nonce, path_id)
}

fn temporary_control_paths_from_parts(
    label: &str,
    process_id: u32,
    nonce: u128,
    path_id: u64,
) -> ControlPaths {
    #[cfg(unix)]
    let temporary_directory = std::path::PathBuf::from("/tmp");
    #[cfg(not(unix))]
    let temporary_directory = std::env::temp_dir();
    let root = temporary_directory.join(format!("mct-{label}-{process_id}-{nonce:x}-{path_id:x}"));
    ControlPaths::from_root(root)
}

#[test]
fn temporary_control_paths_are_distinct_when_timestamps_collide() {
    let first = temporary_control_paths_from_parts("collision", 42, 100, 0);
    let second = temporary_control_paths_from_parts("collision", 42, 100, 1);

    assert_ne!(first.root_directory, second.root_directory);
}
