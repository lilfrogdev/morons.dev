use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::fmt;

const GENERATION_BYTES: usize = 98;
const GENERATION_PREFIX: &str = "rev1_";
const CURSOR_PREFIX: &str = "dif1_";
pub const MAX_DIFF_CURSOR_BYTES: usize = 8_400;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ReviewGeneration([u8; GENERATION_BYTES]);

impl ReviewGeneration {
    pub const fn from_bytes(bytes: [u8; GENERATION_BYTES]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; GENERATION_BYTES] {
        &self.0
    }
}

impl fmt::Debug for ReviewGeneration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ReviewGeneration([OPAQUE])")
    }
}

impl Serialize for ReviewGeneration {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&encode_hex(GENERATION_PREFIX, &self.0))
    }
}

impl<'de> Deserialize<'de> for ReviewGeneration {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        let bytes =
            decode_hex(&value, GENERATION_PREFIX, GENERATION_BYTES).map_err(de::Error::custom)?;
        let bytes: [u8; GENERATION_BYTES] = bytes
            .try_into()
            .map_err(|_| de::Error::custom("invalid review generation"))?;
        Ok(Self(bytes))
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct DiffCursor(String);

impl DiffCursor {
    pub fn from_token(token: String) -> Option<Self> {
        if token.len() <= MAX_DIFF_CURSOR_BYTES
            && token.starts_with(CURSOR_PREFIX)
            && token[CURSOR_PREFIX.len()..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            Some(Self(token))
        } else {
            None
        }
    }

    pub fn as_token(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DiffCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DiffCursor([OPAQUE])")
    }
}

impl Serialize for DiffCursor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for DiffCursor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::from_token(value).ok_or_else(|| de::Error::custom("invalid diff cursor"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffChangeKind {
    Added,
    Modified,
    Deleted,
    ModeChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffNodeKind {
    Directory,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffChange {
    pub path: String,
    pub kind: DiffChangeKind,
    pub old_kind: Option<DiffNodeKind>,
    pub new_kind: Option<DiffNodeKind>,
    pub old_sha256: Option<String>,
    pub new_sha256: Option<String>,
    pub old_bytes: Option<u64>,
    pub new_bytes: Option<u64>,
    pub binary: bool,
    pub excerpt: Option<String>,
}

pub(crate) fn encode_hex(prefix: &str, bytes: &[u8]) -> String {
    let mut value = String::with_capacity(prefix.len() + bytes.len() * 2);
    value.push_str(prefix);
    for byte in bytes {
        use fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

fn decode_hex(value: &str, prefix: &str, bytes: usize) -> Result<Vec<u8>, &'static str> {
    let hex = value.strip_prefix(prefix).ok_or("invalid token prefix")?;
    if hex.len() != bytes * 2 {
        return Err("invalid token length");
    }
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let text = std::str::from_utf8(pair).map_err(|_| "invalid token encoding")?;
            u8::from_str_radix(text, 16).map_err(|_| "invalid token encoding")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_tokens_are_strict_and_debug_redacted() {
        let generation = ReviewGeneration::from_bytes([0xab; GENERATION_BYTES]);
        let encoded = serde_json::to_string(&generation).expect("generation should encode");
        let decoded: ReviewGeneration =
            serde_json::from_str(&encoded).expect("generation should decode");
        assert_eq!(decoded, generation);
        assert!(!format!("{generation:?}").contains("abab"));
        assert!(serde_json::from_str::<ReviewGeneration>("\"rev1_00\"").is_err());

        let cursor = DiffCursor::from_token("dif1_aabb".to_owned()).expect("cursor should form");
        let encoded = serde_json::to_string(&cursor).expect("cursor should encode");
        let decoded: DiffCursor = serde_json::from_str(&encoded).expect("cursor should decode");
        assert_eq!(decoded, cursor);
        assert!(!format!("{cursor:?}").contains("aabb"));
        assert!(DiffCursor::from_token("dif1_not-hex".to_owned()).is_none());
        assert!(DiffCursor::from_token("x".repeat(MAX_DIFF_CURSOR_BYTES + 1)).is_none());
    }
}
