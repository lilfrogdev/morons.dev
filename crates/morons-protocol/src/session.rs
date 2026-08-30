use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

pub const APPLICATION_IDENTIFIER_BYTES: usize = 16;
const CURSOR_BYTES: usize = 8;
const SESSION_ID_PREFIX: &str = "ses_";
const MUTATION_REQUEST_ID_PREFIX: &str = "mut_";
const SESSION_LIST_CURSOR_PREFIX: &str = "sc1_";

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId([u8; APPLICATION_IDENTIFIER_BYTES]);

impl SessionId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; APPLICATION_IDENTIFIER_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; APPLICATION_IDENTIFIER_BYTES] {
        &self.0
    }
}

impl fmt::Debug for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_prefixed_hex(formatter, SESSION_ID_PREFIX, &self.0)
    }
}

impl Serialize for SessionId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&encode_prefixed_hex(SESSION_ID_PREFIX, &self.0))
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        decode_prefixed_hex(&encoded, SESSION_ID_PREFIX)
            .map(Self)
            .map_err(de::Error::custom)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct MutationRequestId([u8; APPLICATION_IDENTIFIER_BYTES]);

impl MutationRequestId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; APPLICATION_IDENTIFIER_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; APPLICATION_IDENTIFIER_BYTES] {
        &self.0
    }
}

impl fmt::Debug for MutationRequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_prefixed_hex(formatter, MUTATION_REQUEST_ID_PREFIX, &self.0)
    }
}

impl Serialize for MutationRequestId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&encode_prefixed_hex(MUTATION_REQUEST_ID_PREFIX, &self.0))
    }
}

impl<'de> Deserialize<'de> for MutationRequestId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        decode_prefixed_hex(&encoded, MUTATION_REQUEST_ID_PREFIX)
            .map(Self)
            .map_err(de::Error::custom)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionListCursor([u8; CURSOR_BYTES]);

impl SessionListCursor {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; CURSOR_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; CURSOR_BYTES] {
        &self.0
    }
}

impl fmt::Debug for SessionListCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_prefixed_hex(formatter, SESSION_LIST_CURSOR_PREFIX, &self.0)
    }
}

impl Serialize for SessionListCursor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&encode_prefixed_hex(SESSION_LIST_CURSOR_PREFIX, &self.0))
    }
}

impl<'de> Deserialize<'de> for SessionListCursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        decode_prefixed_hex(&encoded, SESSION_LIST_CURSOR_PREFIX)
            .map(Self)
            .map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApplicationRequest {
    CreateSession {
        mutation_request_id: MutationRequestId,
        display_name: Option<String>,
    },
    GetSession {
        session_id: SessionId,
    },
    ListSessions {
        cursor: Option<SessionListCursor>,
        limit: u16,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApplicationResponse {
    SessionCreated {
        session: SessionSummary,
    },
    SessionFound {
        session: SessionSummary,
    },
    SessionsListed {
        sessions: Vec<SessionSummary>,
        next_cursor: Option<SessionListCursor>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSummary {
    pub id: SessionId,
    pub display_name: Option<String>,
    pub created_at_milliseconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApplicationError {
    InvalidRequest,
    RequestConflict,
    SessionNotFound,
    ResourceLimit { resource: ResourceLimit },
    ServiceUnavailable,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceLimit {
    Sessions,
    Storage,
}

fn encode_prefixed_hex(prefix: &str, bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(prefix.len() + bytes.len() * 2);
    encoded.push_str(prefix);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn decode_prefixed_hex<const N: usize>(
    encoded: &str,
    prefix: &str,
) -> Result<[u8; N], &'static str> {
    let Some(hex) = encoded.strip_prefix(prefix) else {
        return Err("an opaque identifier has an unexpected prefix");
    };
    if hex.len() != N * 2 {
        return Err("an opaque identifier has an unexpected length");
    }

    let mut decoded = [0_u8; N];
    let (pairs, _) = hex.as_bytes().as_chunks::<2>();
    for (index, pair) in pairs.iter().enumerate() {
        let high = decode_hex_digit(pair[0])?;
        let low = decode_hex_digit(pair[1])?;
        decoded[index] = high << 4 | low;
    }
    Ok(decoded)
}

fn decode_hex_digit(byte: u8) -> Result<u8, &'static str> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err("an opaque identifier must use lowercase hexadecimal digits"),
    }
}

fn write_prefixed_hex(
    formatter: &mut fmt::Formatter<'_>,
    prefix: &str,
    bytes: &[u8],
) -> fmt::Result {
    formatter.write_str(prefix)?;
    for byte in bytes {
        write!(formatter, "{byte:02x}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde::Serialize;
    use serde_json::{Value, json};

    use super::{
        ApplicationError, ApplicationRequest, ApplicationResponse, MutationRequestId,
        ResourceLimit, SessionId, SessionListCursor, SessionSummary,
    };

    #[test]
    fn application_request_has_stable_json_shape() {
        let request = ApplicationRequest::CreateSession {
            mutation_request_id: MutationRequestId::from_bytes([0x11; 16]),
            display_name: Some("A session".to_owned()),
        };
        let actual = serde_json::to_value(request).expect("request should encode");

        assert_eq!(
            actual,
            json!({
                "operation": "create_session",
                "mutation_request_id": "mut_11111111111111111111111111111111",
                "display_name": "A session",
            })
        );
    }

    #[test]
    fn application_response_has_stable_json_shape() {
        let response = ApplicationResponse::SessionsListed {
            sessions: vec![SessionSummary {
                id: SessionId::from_bytes([0x22; 16]),
                display_name: None,
                created_at_milliseconds: 42,
            }],
            next_cursor: Some(SessionListCursor::from_bytes(7_u64.to_be_bytes())),
        };
        let actual = serde_json::to_value(response).expect("response should encode");

        assert_eq!(
            actual,
            json!({
                "result": "sessions_listed",
                "sessions": [{
                    "id": "ses_22222222222222222222222222222222",
                    "display_name": null,
                    "created_at_milliseconds": 42,
                }],
                "next_cursor": "sc1_0000000000000007",
            })
        );
    }

    #[test]
    fn application_error_has_stable_json_shape() {
        let error = ApplicationError::ResourceLimit {
            resource: ResourceLimit::Sessions,
        };
        let actual = serde_json::to_value(error).expect("error should encode");

        assert_eq!(
            actual,
            json!({
                "code": "resource_limit",
                "resource": "sessions",
            })
        );
    }

    #[test]
    fn opaque_values_round_trip_through_json() {
        let session_id = SessionId::from_bytes([0x33; 16]);
        let mutation_id = MutationRequestId::from_bytes([0x44; 16]);
        let cursor = SessionListCursor::from_bytes([0x55; 8]);

        assert_eq!(round_trip(&session_id), session_id);
        assert_eq!(round_trip(&mutation_id), mutation_id);
        assert_eq!(round_trip(&cursor), cursor);
    }

    #[test]
    fn malformed_opaque_values_are_rejected() {
        for encoded in [
            "ses_1111111111111111111111111111111",
            "ses_1111111111111111111111111111111g",
            "ses_1111111111111111111111111111111A",
            "mut_11111111111111111111111111111111",
        ] {
            assert!(
                serde_json::from_value::<SessionId>(Value::String(encoded.to_owned())).is_err()
            );
        }
    }

    #[test]
    fn application_request_rejects_unknown_fields() {
        let encoded = json!({
            "operation": "list_sessions",
            "cursor": null,
            "limit": 10,
            "extra": true,
        });

        assert!(serde_json::from_value::<ApplicationRequest>(encoded).is_err());
    }

    fn round_trip<T>(value: &T) -> T
    where
        T: Serialize + serde::de::DeserializeOwned,
    {
        let encoded = serde_json::to_vec(value).expect("value should encode");
        serde_json::from_slice(&encoded).expect("value should decode")
    }
}
