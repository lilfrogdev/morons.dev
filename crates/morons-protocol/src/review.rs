use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::fmt;

const TOKEN_BYTES: usize = 32;
const TOKEN_PREFIX: &str = "rev1_";
pub const MAX_EXPORT_DESTINATION_PATH_BYTES: usize = 4096;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ReviewGeneration([u8; TOKEN_BYTES]);
impl ReviewGeneration {
    pub const fn from_bytes(bytes: [u8; TOKEN_BYTES]) -> Self {
        Self(bytes)
    }
    pub const fn as_bytes(&self) -> &[u8; TOKEN_BYTES] {
        &self.0
    }
}
impl fmt::Debug for ReviewGeneration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ReviewGeneration([OPAQUE])")
    }
}
impl Serialize for ReviewGeneration {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut value = String::from(TOKEN_PREFIX);
        for byte in self.0 {
            value.push_str(&format!("{byte:02x}"));
        }
        s.serialize_str(&value)
    }
}
impl<'de> Deserialize<'de> for ReviewGeneration {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = String::deserialize(d)?;
        let hex = value
            .strip_prefix(TOKEN_PREFIX)
            .ok_or_else(|| de::Error::custom("invalid review token"))?;
        if hex.len() != TOKEN_BYTES * 2 {
            return Err(de::Error::custom("invalid review token"));
        }
        let mut bytes = [0_u8; TOKEN_BYTES];
        for (index, pair) in hex.as_bytes().as_chunks::<2>().0.iter().enumerate() {
            let text = std::str::from_utf8(pair).map_err(de::Error::custom)?;
            bytes[index] = u8::from_str_radix(text, 16).map_err(de::Error::custom)?;
        }
        Ok(Self(bytes))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffCursor {
    pub generation: ReviewGeneration,
    pub after_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffChangeKind {
    Added,
    Modified,
    Deleted,
    ModeChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffChange {
    pub path: String,
    pub kind: DiffChangeKind,
    pub old_sha256: Option<String>,
    pub new_sha256: Option<String>,
    pub old_bytes: Option<u64>,
    pub new_bytes: Option<u64>,
    pub binary: bool,
    pub excerpt: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportSummary {
    pub file_count: u64,
    pub directory_count: u64,
    pub logical_bytes: u64,
}
