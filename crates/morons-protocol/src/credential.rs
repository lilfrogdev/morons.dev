use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use zeroize::Zeroize;

pub const MAX_OPENCODE_API_KEY_BYTES: usize = 4096;

pub struct OpenCodeApiKey(Vec<u8>);

impl OpenCodeApiKey {
    pub fn new(value: impl Into<Vec<u8>>) -> Result<Self, OpenCodeApiKeyError> {
        Self::from_bytes(value.into())
    }

    #[must_use]
    pub fn into_bytes(mut self) -> Vec<u8> {
        std::mem::take(&mut self.0)
    }

    fn from_bytes(mut bytes: Vec<u8>) -> Result<Self, OpenCodeApiKeyError> {
        if bytes.is_empty()
            || bytes.len() > MAX_OPENCODE_API_KEY_BYTES
            || !bytes.iter().all(|byte| matches!(byte, 0x21..=0x7e))
        {
            bytes.zeroize();
            return Err(OpenCodeApiKeyError);
        }
        Ok(Self(bytes))
    }
}

impl Clone for OpenCodeApiKey {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl fmt::Debug for OpenCodeApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpenCodeApiKey([REDACTED])")
    }
}

impl PartialEq for OpenCodeApiKey {
    fn eq(&self, other: &Self) -> bool {
        constant_time_eq(&self.0, &other.0)
    }
}

impl Eq for OpenCodeApiKey {}

impl Drop for OpenCodeApiKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl Serialize for OpenCodeApiKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let value = std::str::from_utf8(&self.0).map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(value)
    }
}

impl<'de> Deserialize<'de> for OpenCodeApiKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ApiKeyVisitor;

        impl<'de> de::Visitor<'de> for ApiKeyVisitor {
            type Value = OpenCodeApiKey;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a bounded visible-ASCII OpenCode API key")
            }

            fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                OpenCodeApiKey::from_bytes(value.as_bytes().to_vec()).map_err(E::custom)
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                OpenCodeApiKey::from_bytes(value.as_bytes().to_vec()).map_err(E::custom)
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                OpenCodeApiKey::from_bytes(value.into_bytes()).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(ApiKeyVisitor)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenCodeApiKeyError;

impl fmt::Display for OpenCodeApiKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an OpenCode API key is invalid")
    }
}

impl std::error::Error for OpenCodeApiKeyError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenCodeCredentialStatus {
    pub configured: bool,
    pub generation: u64,
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_KEY: &str = "not-a-real-opencode-key";

    #[test]
    fn api_key_round_trips_and_debug_is_redacted() {
        let key = OpenCodeApiKey::new(TEST_KEY).expect("test key should be valid");
        let encoded = serde_json::to_vec(&key).expect("API key should encode");
        let decoded: OpenCodeApiKey =
            serde_json::from_slice(&encoded).expect("API key should decode");

        assert_eq!(decoded, key);
        assert_eq!(format!("{key:?}"), "OpenCodeApiKey([REDACTED])");
        assert!(!format!("{key:?}").contains(TEST_KEY));
    }

    #[test]
    fn api_key_rejects_invalid_values() {
        for value in [
            Vec::new(),
            b"contains space".to_vec(),
            b"line\nbreak".to_vec(),
        ] {
            assert!(OpenCodeApiKey::new(value).is_err());
        }
        assert!(OpenCodeApiKey::new(vec![b'x'; MAX_OPENCODE_API_KEY_BYTES + 1]).is_err());
        assert!(OpenCodeApiKey::new(vec![0x80]).is_err());
    }

    #[test]
    fn credential_status_rejects_unknown_fields() {
        let payload = br#"{"configured":false,"generation":0,"unexpected":true}"#;
        assert!(serde_json::from_slice::<OpenCodeCredentialStatus>(payload).is_err());
    }
}
