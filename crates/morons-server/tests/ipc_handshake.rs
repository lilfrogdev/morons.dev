use std::{
    io, process,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use interprocess::local_socket::{
    ListenerOptions, Name,
    tokio::{Stream, prelude::*},
};
use morons_cli::perform_handshake;
use morons_protocol::{
    AUTHENTICATION_KEY_BYTES, AuthenticationKey, HOST_EPOCH_BYTES, HostEpoch, authenticate_client,
    authenticate_server, authorize_accepted_peer, verify_connected_server_peer,
};
use morons_server::handle_handshake;

#[cfg(unix)]
use {interprocess::local_socket::GenericFilePath, std::path::PathBuf};

#[cfg(windows)]
use interprocess::local_socket::GenericNamespaced;

const TEST_CLIENT_VERSION: &str = "test-client-version";
const TEST_SERVER_VERSION: &str = "test-server-version";
const TEST_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::test(flavor = "current_thread")]
async fn client_and_server_authenticate_before_protocol_handshake() {
    let name = test_socket_name().expect("test socket name should be valid");
    let listener = ListenerOptions::new()
        .name(name.clone())
        .create_tokio()
        .expect("test listener should be created");
    let host_epoch = HostEpoch::from_bytes([0x22; HOST_EPOCH_BYTES]);

    let exchange = async move {
        let server = async {
            let mut connection = listener
                .accept()
                .await
                .expect("server should accept a connection");
            authorize_accepted_peer(&connection).expect("server should authorize client peer");

            let key = AuthenticationKey::from_bytes([0x11; AUTHENTICATION_KEY_BYTES]);
            authenticate_server(&mut connection, &key, &host_epoch)
                .await
                .expect("server should authenticate client");
            handle_handshake(&mut connection, TEST_SERVER_VERSION)
                .await
                .expect("server handshake should succeed");
        };

        let client = async {
            let mut connection = Stream::connect(name)
                .await
                .expect("client should connect to the server");
            verify_connected_server_peer(&connection, process::id())
                .expect("client should verify server peer");

            let key = AuthenticationKey::from_bytes([0x11; AUTHENTICATION_KEY_BYTES]);
            authenticate_client(&mut connection, &key, &host_epoch)
                .await
                .expect("client should authenticate server");
            perform_handshake(&mut connection, TEST_CLIENT_VERSION).await
        };

        let ((), client_result) = tokio::join!(server, client);

        client_result.expect("client handshake should succeed")
    };

    let server_version = tokio::time::timeout(TEST_TIMEOUT, exchange)
        .await
        .expect("IPC authentication and handshake should not time out");

    assert_eq!(server_version, TEST_SERVER_VERSION);
}

fn test_socket_name() -> io::Result<Name<'static>> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let unique_name = format!("morons-ipc-test-{}-{nonce}", process::id());

    #[cfg(unix)]
    {
        PathBuf::from("/tmp")
            .join(format!("{unique_name}.sock"))
            .to_fs_name::<GenericFilePath>()
    }

    #[cfg(windows)]
    {
        unique_name.to_ns_name::<GenericNamespaced>()
    }

    #[cfg(not(any(unix, windows)))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "local sockets are unsupported on this platform",
        ))
    }
}
