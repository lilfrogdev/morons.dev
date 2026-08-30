use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{ApplicationError, ApplicationRequest, ApplicationResponse, PROTOCOL_VERSION};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMessage {
    Hello {
        protocol_version: u32,
        client_version: String,
    },
    Request {
        request_id: u64,
        request: ApplicationRequest,
    },
}

impl ClientMessage {
    #[must_use]
    pub fn hello(client_version: impl Into<String>) -> Self {
        Self::Hello {
            protocol_version: PROTOCOL_VERSION,
            client_version: client_version.into(),
        }
    }

    #[must_use]
    pub const fn request(request_id: u64, request: ApplicationRequest) -> Self {
        Self::Request {
            request_id,
            request,
        }
    }

    pub fn encode_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        encode_json(self)
    }

    pub fn decode_json(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        decode_json(bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerMessage {
    Hello {
        protocol_version: u32,
        server_version: String,
    },
    ProtocolVersionMismatch {
        expected_protocol_version: u32,
        received_protocol_version: u32,
    },
    Response {
        request_id: u64,
        response: ApplicationResponse,
    },
    RequestFailed {
        request_id: u64,
        error: ApplicationError,
    },
}

impl ServerMessage {
    #[must_use]
    pub fn hello(server_version: impl Into<String>) -> Self {
        Self::Hello {
            protocol_version: PROTOCOL_VERSION,
            server_version: server_version.into(),
        }
    }

    #[must_use]
    pub const fn protocol_version_mismatch(received_protocol_version: u32) -> Self {
        Self::ProtocolVersionMismatch {
            expected_protocol_version: PROTOCOL_VERSION,
            received_protocol_version,
        }
    }

    #[must_use]
    pub const fn response(request_id: u64, response: ApplicationResponse) -> Self {
        Self::Response {
            request_id,
            response,
        }
    }

    #[must_use]
    pub const fn request_failed(request_id: u64, error: ApplicationError) -> Self {
        Self::RequestFailed { request_id, error }
    }

    pub fn encode_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        encode_json(self)
    }

    pub fn decode_json(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        decode_json(bytes)
    }
}

fn encode_json<T: Serialize>(value: &T) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(value)
}

fn decode_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, serde_json::Error> {
    serde_json::from_slice(bytes)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{ClientMessage, ServerMessage};
    use crate::{
        ApplicationError, ApplicationRequest, ApplicationResponse, MutationRequestId,
        PROTOCOL_VERSION, SessionId, SessionSummary,
    };

    const TEST_CLIENT_VERSION: &str = "test-client-version";

    #[test]
    fn client_hello_has_stable_json_shape() {
        let message = ClientMessage::hello(TEST_CLIENT_VERSION);
        let encoded = message.encode_json().expect("client hello should encode");
        let actual: Value =
            serde_json::from_slice(&encoded).expect("encoded message should be JSON");

        assert_eq!(
            actual,
            json!({
                "type": "hello",
                "protocol_version": PROTOCOL_VERSION,
                "client_version": TEST_CLIENT_VERSION,
            })
        );
    }

    #[test]
    fn application_messages_have_stable_json_shapes() {
        let request = ClientMessage::request(
            7,
            ApplicationRequest::CreateSession {
                mutation_request_id: MutationRequestId::from_bytes([0x11; 16]),
                display_name: None,
            },
        );
        let response = ServerMessage::response(
            7,
            ApplicationResponse::SessionCreated {
                session: SessionSummary {
                    id: SessionId::from_bytes([0x22; 16]),
                    display_name: None,
                    created_at_milliseconds: 42,
                },
            },
        );
        let failure = ServerMessage::request_failed(7, ApplicationError::RequestConflict);

        assert_eq!(
            serde_json::to_value(request).expect("request should encode"),
            json!({
                "type": "request",
                "request_id": 7,
                "request": {
                    "operation": "create_session",
                    "mutation_request_id": "mut_11111111111111111111111111111111",
                    "display_name": null,
                },
            })
        );
        assert_eq!(
            serde_json::to_value(response).expect("response should encode"),
            json!({
                "type": "response",
                "request_id": 7,
                "response": {
                    "result": "session_created",
                    "session": {
                        "id": "ses_22222222222222222222222222222222",
                        "display_name": null,
                        "created_at_milliseconds": 42,
                    },
                },
            })
        );
        assert_eq!(
            serde_json::to_value(failure).expect("failure should encode"),
            json!({
                "type": "request_failed",
                "request_id": 7,
                "error": { "code": "request_conflict" },
            })
        );
    }

    #[test]
    fn messages_round_trip_through_json() {
        let client = ClientMessage::request(
            9,
            ApplicationRequest::GetSession {
                session_id: SessionId::from_bytes([0x33; 16]),
            },
        );
        let server = ServerMessage::request_failed(9, ApplicationError::SessionNotFound);

        assert_eq!(
            ClientMessage::decode_json(&client.encode_json().expect("message should encode"))
                .expect("message should decode"),
            client
        );
        assert_eq!(
            ServerMessage::decode_json(&server.encode_json().expect("message should encode"))
                .expect("message should decode"),
            server
        );
    }

    #[test]
    fn client_message_rejects_unknown_fields() {
        let encoded = serde_json::to_vec(&json!({
            "type": "hello",
            "protocol_version": PROTOCOL_VERSION,
            "client_version": TEST_CLIENT_VERSION,
            "extra": true,
        }))
        .expect("test message should encode");

        assert!(ClientMessage::decode_json(&encoded).is_err());
    }

    #[test]
    fn protocol_version_mismatch_has_stable_json_shape() {
        let received_protocol_version = PROTOCOL_VERSION + 1;
        let message = ServerMessage::protocol_version_mismatch(received_protocol_version);
        let encoded = message.encode_json().expect("message should encode");
        let actual: Value =
            serde_json::from_slice(&encoded).expect("encoded message should be JSON");

        assert_eq!(
            actual,
            json!({
                "type": "protocol_version_mismatch",
                "expected_protocol_version": PROTOCOL_VERSION,
                "received_protocol_version": received_protocol_version,
            })
        );
    }
}
