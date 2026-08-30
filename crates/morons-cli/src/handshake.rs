use std::{error::Error, fmt};

use morons_protocol::{
    ClientMessage, FrameError, PROTOCOL_VERSION, ServerMessage, read_server_message,
    write_client_message,
};
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Debug)]
#[non_exhaustive]
pub enum HandshakeError {
    Frame(FrameError),
    ServerDisconnected,
    ProtocolVersionMismatch {
        expected_protocol_version: u32,
        received_protocol_version: u32,
    },
    UnexpectedServerMessage,
}

impl fmt::Display for HandshakeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(error) => write!(formatter, "handshake frame failed: {error}"),
            Self::ServerDisconnected => write!(formatter, "server disconnected during handshake"),
            Self::ProtocolVersionMismatch {
                expected_protocol_version,
                received_protocol_version,
            } => write!(
                formatter,
                "protocol version mismatch: expected {expected_protocol_version}, received {received_protocol_version}"
            ),
            Self::UnexpectedServerMessage => {
                formatter.write_str("server sent an application message during the handshake")
            }
        }
    }
}

impl Error for HandshakeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Frame(error) => Some(error),
            Self::ServerDisconnected
            | Self::ProtocolVersionMismatch { .. }
            | Self::UnexpectedServerMessage => None,
        }
    }
}

impl From<FrameError> for HandshakeError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

pub async fn perform_handshake<S>(
    connection: &mut S,
    client_version: &str,
) -> Result<String, HandshakeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    write_client_message(connection, &ClientMessage::hello(client_version)).await?;

    let response = read_server_message(connection)
        .await?
        .ok_or(HandshakeError::ServerDisconnected)?;

    match response {
        ServerMessage::Hello {
            protocol_version,
            server_version,
        } if protocol_version == PROTOCOL_VERSION => Ok(server_version),
        ServerMessage::Hello {
            protocol_version, ..
        } => Err(HandshakeError::ProtocolVersionMismatch {
            expected_protocol_version: PROTOCOL_VERSION,
            received_protocol_version: protocol_version,
        }),
        ServerMessage::ProtocolVersionMismatch {
            expected_protocol_version,
            received_protocol_version,
        } => Err(HandshakeError::ProtocolVersionMismatch {
            expected_protocol_version,
            received_protocol_version,
        }),
        ServerMessage::Response { .. }
        | ServerMessage::RequestFailed { .. }
        | ServerMessage::Event { .. }
        | ServerMessage::SubscriptionEnded { .. } => Err(HandshakeError::UnexpectedServerMessage),
    }
}

#[cfg(test)]
mod tests {
    use morons_protocol::{
        ApplicationError, ClientMessage, PROTOCOL_VERSION, ServerMessage, read_client_message,
        write_server_message,
    };

    use super::{HandshakeError, perform_handshake};

    const TEST_CLIENT_VERSION: &str = "test-client-version";
    const TEST_SERVER_VERSION: &str = "test-server-version";

    #[tokio::test(flavor = "current_thread")]
    async fn valid_server_hello_is_accepted() {
        let (mut client, mut server) = tokio::io::duplex(1024);

        let client_handshake = perform_handshake(&mut client, TEST_CLIENT_VERSION);
        let server_handshake = async {
            let request = read_client_message(&mut server)
                .await
                .expect("client request should be read")
                .expect("client should send a request");

            assert_eq!(request, ClientMessage::hello(TEST_CLIENT_VERSION));

            write_server_message(&mut server, &ServerMessage::hello(TEST_SERVER_VERSION))
                .await
                .expect("server response should be written");
        };

        let (client_result, ()) = tokio::join!(client_handshake, server_handshake);

        assert_eq!(
            client_result.expect("handshake should succeed"),
            TEST_SERVER_VERSION
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn server_protocol_mismatch_is_reported() {
        let expected_protocol_version = PROTOCOL_VERSION + 1;
        let (mut client, mut server) = tokio::io::duplex(1024);

        let client_handshake = perform_handshake(&mut client, TEST_CLIENT_VERSION);
        let server_handshake = async {
            read_client_message(&mut server)
                .await
                .expect("client request should be read")
                .expect("client should send a request");

            let response = ServerMessage::ProtocolVersionMismatch {
                expected_protocol_version,
                received_protocol_version: PROTOCOL_VERSION,
            };

            write_server_message(&mut server, &response)
                .await
                .expect("server response should be written");
        };

        let (client_result, ()) = tokio::join!(client_handshake, server_handshake);
        let error = client_result.expect_err("handshake should reject mismatched protocol");

        assert!(matches!(
                error,
                HandshakeError::ProtocolVersionMismatch {
                    expected_protocol_version: expected,
                    received_protocol_version: received,
                } if expected == expected_protocol_version && received == PROTOCOL_VERSION
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unexpected_server_protocol_version_is_rejected() {
        let received_protocol_version = PROTOCOL_VERSION + 1;
        let response = ServerMessage::Hello {
            protocol_version: received_protocol_version,
            server_version: TEST_SERVER_VERSION.to_owned(),
        };
        let (mut client, mut server) = tokio::io::duplex(1024);

        let client_handshake = perform_handshake(&mut client, TEST_CLIENT_VERSION);
        let server_handshake = async {
            read_client_message(&mut server)
                .await
                .expect("client request should be read")
                .expect("client should send a request");

            write_server_message(&mut server, &response)
                .await
                .expect("server response should be written");
        };

        let (client_result, ()) = tokio::join!(client_handshake, server_handshake);
        let error = client_result.expect_err("unexpected server protocol should be rejected");

        assert!(matches!(
                error,
                HandshakeError::ProtocolVersionMismatch {
                    expected_protocol_version: PROTOCOL_VERSION,
                    received_protocol_version,
                } if received_protocol_version == PROTOCOL_VERSION + 1
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn application_message_during_handshake_is_rejected() {
        let (mut client, mut server) = tokio::io::duplex(1024);

        let client_handshake = perform_handshake(&mut client, TEST_CLIENT_VERSION);
        let server_handshake = async {
            read_client_message(&mut server)
                .await
                .expect("client request should be read")
                .expect("client should send a request");
            write_server_message(
                &mut server,
                &ServerMessage::request_failed(1, ApplicationError::Internal),
            )
            .await
            .expect("server response should be written");
        };

        let (client_result, ()) = tokio::join!(client_handshake, server_handshake);
        assert!(matches!(
            client_result.expect_err("application message should fail handshake"),
            HandshakeError::UnexpectedServerMessage
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn server_disconnect_during_handshake_is_reported() {
        let (mut client, mut server) = tokio::io::duplex(1024);

        let client_handshake = perform_handshake(&mut client, TEST_CLIENT_VERSION);
        let server_handshake = async move {
            read_client_message(&mut server)
                .await
                .expect("client request should be read")
                .expect("client should send a request");
        };

        let (client_result, ()) = tokio::join!(client_handshake, server_handshake);
        let error = client_result.expect_err("server disconnect should fail the handshake");

        assert!(matches!(error, HandshakeError::ServerDisconnected));
    }
}
