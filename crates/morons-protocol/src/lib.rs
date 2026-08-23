use serde::{Deserialize, Serialize, de::DeserializeOwned};
mod framing;

pub use framing::{
    FrameError, MAX_FRAME_PAYLOAD_BYTES, read_client_message, read_server_message,
    write_client_message, write_server_message,
};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMessage {
    Hello {
        protocol_version: u32,
        client_version: String,
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
    pub fn protocol_version_mismatch(received_protocol_version: u32) -> Self {
        Self::ProtocolVersionMismatch {
            expected_protocol_version: PROTOCOL_VERSION,
            received_protocol_version,
        }
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

    use super::{ClientMessage, PROTOCOL_VERSION, ServerMessage};

    const TEST_CLIENT_VERSION: &str = "test-client-version";
    const TEST_SERVER_VERSION: &str = "test-server-version";

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
    fn client_message_round_trips_through_json() {
        let expected = ClientMessage::hello(TEST_CLIENT_VERSION);
        let encoded = expected.encode_json().expect("message should encode");
        let actual = ClientMessage::decode_json(&encoded).expect("message should decode");

        assert_eq!(actual, expected);
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
    fn server_reports_protocol_version_mismatch() {
        let received_protocol_version = PROTOCOL_VERSION + 1;
        let message = ServerMessage::protocol_version_mismatch(received_protocol_version);

        assert_eq!(
            message,
            ServerMessage::ProtocolVersionMismatch {
                expected_protocol_version: PROTOCOL_VERSION,
                received_protocol_version,
            }
        );
    }

    #[test]
    fn protocol_version_mismatch_has_stable_json_shape() {
        let received_protocol_version = PROTOCOL_VERSION + 1;
        let message = ServerMessage::protocol_version_mismatch(received_protocol_version);
        let encoded = message.encode_json().expect("server message should encode");
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

    #[test]
    fn server_message_round_trips_through_json() {
        let expected = ServerMessage::hello(TEST_SERVER_VERSION);
        let encoded = expected.encode_json().expect("message should encode");
        let actual = ServerMessage::decode_json(&encoded).expect("message should decode");

        assert_eq!(actual, expected);
    }
}
