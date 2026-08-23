use std::{
    io, process,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use interprocess::local_socket::{
    ListenerOptions, Name,
    tokio::{Stream, prelude::*},
};
use morons_cli::perform_handshake;
use morons_server::handle_handshake;

#[cfg(unix)]
use {interprocess::local_socket::GenericFilePath, std::path::PathBuf};

#[cfg(windows)]
use interprocess::local_socket::GenericNamespaced;

const TEST_CLIENT_VERSION: &str = "test-client-version";
const TEST_SERVER_VERSION: &str = "test-server-version";
const TEST_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::test(flavor = "current_thread")]
async fn client_and_server_complete_handshake_over_local_socket() {
    let name = test_socket_name().expect("test socket name should be valid");
    let listener = ListenerOptions::new()
        .name(name.clone())
        .create_tokio()
        .expect("test listener should be created");

    let exchange = async move {
        let server = async {
            let mut connection = listener
                .accept()
                .await
                .expect("server should accept a connection");

            handle_handshake(&mut connection, TEST_SERVER_VERSION).await
        };

        let client = async {
            let mut connection = Stream::connect(name)
                .await
                .expect("client should connect to the server");

            perform_handshake(&mut connection, TEST_CLIENT_VERSION).await
        };

        let (server_result, client_result) = tokio::join!(server, client);

        server_result.expect("server handshake should succeed");
        client_result.expect("client handshake should succeed")
    };

    let server_version = tokio::time::timeout(TEST_TIMEOUT, exchange)
        .await
        .expect("IPC handshake should not time out");

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
