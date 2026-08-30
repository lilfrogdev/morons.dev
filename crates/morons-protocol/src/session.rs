use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

pub const APPLICATION_IDENTIFIER_BYTES: usize = 16;
const SESSION_LIST_CURSOR_BYTES: usize = 16;
const SESSION_CATALOG_CURSOR_BYTES: usize = 8;
const SESSION_ID_PREFIX: &str = "ses_";
const MUTATION_REQUEST_ID_PREFIX: &str = "mut_";
const SESSION_LIST_CURSOR_PREFIX: &str = "sc2_";
const SESSION_CATALOG_CURSOR_PREFIX: &str = "scc1_";

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
pub struct SessionListCursor([u8; SESSION_LIST_CURSOR_BYTES]);

impl SessionListCursor {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; SESSION_LIST_CURSOR_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; SESSION_LIST_CURSOR_BYTES] {
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

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionCatalogEventCursor([u8; SESSION_CATALOG_CURSOR_BYTES]);

impl SessionCatalogEventCursor {
    #[must_use]
    pub const fn beginning() -> Self {
        Self([0; SESSION_CATALOG_CURSOR_BYTES])
    }

    #[must_use]
    pub const fn from_bytes(bytes: [u8; SESSION_CATALOG_CURSOR_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; SESSION_CATALOG_CURSOR_BYTES] {
        &self.0
    }
}

impl fmt::Debug for SessionCatalogEventCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_prefixed_hex(formatter, SESSION_CATALOG_CURSOR_PREFIX, &self.0)
    }
}

impl Serialize for SessionCatalogEventCursor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&encode_prefixed_hex(SESSION_CATALOG_CURSOR_PREFIX, &self.0))
    }
}

impl<'de> Deserialize<'de> for SessionCatalogEventCursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        decode_prefixed_hex(&encoded, SESSION_CATALOG_CURSOR_PREFIX)
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
    SubscribeSessionCatalog {
        cursor: SessionCatalogEventCursor,
    },
    GetOpenCodeCredentialStatus,
    SetOpenCodeCredential {
        mutation_request_id: MutationRequestId,
        expected_generation: u64,
        api_key: crate::OpenCodeApiKey,
    },
    RemoveOpenCodeCredential {
        mutation_request_id: MutationRequestId,
        expected_generation: u64,
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
        catalog_cursor: SessionCatalogEventCursor,
    },
    SessionCatalogSubscriptionStarted {
        cursor: SessionCatalogEventCursor,
    },
    OpenCodeCredentialStatus {
        credential: crate::OpenCodeCredentialStatus,
    },
    OpenCodeCredentialUpdated {
        credential: crate::OpenCodeCredentialStatus,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApplicationEvent {
    SessionCreated {
        cursor: SessionCatalogEventCursor,
        session: SessionSummary,
    },
}

impl ApplicationEvent {
    #[must_use]
    pub const fn cursor(&self) -> SessionCatalogEventCursor {
        match self {
            Self::SessionCreated { cursor, .. } => *cursor,
        }
    }
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
    CredentialGenerationConflict,
    CredentialMutationNotApplied,
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
mod tests;
