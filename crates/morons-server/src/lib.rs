use morons_protocol::{
    ClientMessage, FrameError, PROTOCOL_VERSION, ServerMessage, read_client_message,
    write_server_message,
};
use tokio::io::{AsyncRead, AsyncWrite};

/// Handles one authoritative server-side protocol handshake.
pub async fn handle_handshake<S>(connection: &mut S, server_version: &str) -> Result<(), FrameError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let Some(message) = read_client_message(connection).await? else {
        return Ok(());
    };

    let response = match message {
        ClientMessage::Hello {
            protocol_version, ..
        } if protocol_version == PROTOCOL_VERSION => ServerMessage::hello(server_version),
        ClientMessage::Hello {
            protocol_version, ..
        } => ServerMessage::protocol_version_mismatch(protocol_version),
    };

    write_server_message(connection, &response).await
}

#[cfg(test)]
mod tests {
    use super::handle_handshake;
    use morons_protocol::{
        ClientMessage, PROTOCOL_VERSION, ServerMessage, read_server_message, write_client_message,
    };

    const TEST_CLIENT_VERSION: &str = "test-client-version";
    const TEST_SERVER_VERSION: &str = "test-server-version";

    #[tokio::test(flavor = "current_thread")]
    async fn matching_protocol_version_is_accepted() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        let request = ClientMessage::hello(TEST_CLIENT_VERSION);

        write_client_message(&mut client, &request)
            .await
            .expect("client hello should be written");

        handle_handshake(&mut server, TEST_SERVER_VERSION)
            .await
            .expect("server handshake succeeded");

        let response = read_server_message(&mut client)
            .await
            .expect("server response should be read")
            .expect("server should send a response");

        assert_eq!(response, ServerMessage::hello(TEST_SERVER_VERSION));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mismatched_protocol_version_is_reported() {
        let received_protocol_version = PROTOCOL_VERSION + 1;
        let request = ClientMessage::Hello {
            protocol_version: received_protocol_version,
            client_version: TEST_CLIENT_VERSION.to_owned(),
        };
        let (mut client, mut server) = tokio::io::duplex(1024);

        write_client_message(&mut client, &request)
            .await
            .expect("client hello should be written");

        handle_handshake(&mut server, TEST_SERVER_VERSION)
            .await
            .expect("server should report mismatch");

        let response = read_server_message(&mut client)
            .await
            .expect("server response should be read")
            .expect("server should send a response");

        assert_eq!(
            response,
            ServerMessage::ProtocolVersionMismatch {
                expected_protocol_version: PROTOCOL_VERSION,
                received_protocol_version,
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn disconnect_before_hello_is_clean() {
        let (client, mut server) = tokio::io::duplex(64);
        drop(client);

        handle_handshake(&mut server, TEST_SERVER_VERSION)
            .await
            .expect("client disconnect should be handled cleanly");
    }
}
