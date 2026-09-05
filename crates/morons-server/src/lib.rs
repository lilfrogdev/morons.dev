mod application;
mod command_supervisor;
mod connection;
mod persistence;
mod project_context;
mod prompts;
pub mod provider;
mod run_supervisor;
mod skills;
mod tools;

pub use application::{ApplicationStartupError, ServerApplication};
pub use connection::{
    ConnectionError, HandshakeOutcome, handle_handshake, handle_local_owner_requests,
};

#[cfg(test)]
mod tests {
    use morons_protocol::{
        ApplicationRequest, ClientMessage, MutationRequestId, PROTOCOL_VERSION, ServerMessage,
        read_server_message, write_client_message,
    };

    use super::{ConnectionError, HandshakeOutcome, handle_handshake};

    const TEST_CLIENT_VERSION: &str = "test-client-version";
    const TEST_SERVER_VERSION: &str = "test-server-version";

    #[tokio::test(flavor = "current_thread")]
    async fn matching_protocol_version_is_accepted() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        let request = ClientMessage::hello(TEST_CLIENT_VERSION);

        write_client_message(&mut client, &request)
            .await
            .expect("client hello should be written");

        let outcome = handle_handshake(&mut server, TEST_SERVER_VERSION)
            .await
            .expect("server handshake succeeded");
        assert_eq!(outcome, HandshakeOutcome::Accepted);

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

        let outcome = handle_handshake(&mut server, TEST_SERVER_VERSION)
            .await
            .expect("server should report mismatch");
        assert_eq!(outcome, HandshakeOutcome::Rejected);

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
    async fn disconnect_before_hello_is_clean_rejection() {
        let (client, mut server) = tokio::io::duplex(64);
        drop(client);

        let outcome = handle_handshake(&mut server, TEST_SERVER_VERSION)
            .await
            .expect("client disconnect should be handled cleanly");

        assert_eq!(outcome, HandshakeOutcome::Rejected);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn application_request_before_hello_is_rejected() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        let request = ClientMessage::request(
            1,
            ApplicationRequest::CreateSession {
                mutation_request_id: MutationRequestId::from_bytes([0x11; 16]),
                display_name: None,
                working_directory: "/projects/example".to_owned(),
            },
        );
        write_client_message(&mut client, &request)
            .await
            .expect("client request should be written");

        let error = handle_handshake(&mut server, TEST_SERVER_VERSION)
            .await
            .expect_err("application request before hello should fail");

        assert!(matches!(error, ConnectionError::UnexpectedClientMessage));
    }
}
