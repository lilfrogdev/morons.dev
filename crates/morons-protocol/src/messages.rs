use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    ApplicationError, ApplicationEvent, ApplicationRequest, ApplicationResponse, PROTOCOL_VERSION,
};

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
    Event {
        event: ApplicationEvent,
    },
    SubscriptionEnded {
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

    #[must_use]
    pub const fn event(event: ApplicationEvent) -> Self {
        Self::Event { event }
    }

    #[must_use]
    pub const fn subscription_ended(error: ApplicationError) -> Self {
        Self::SubscriptionEnded { error }
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
mod tests;
