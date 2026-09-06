use std::collections::BTreeSet;

use serde::Deserialize;

use super::{OpenCodeModel, OpenCodeService, ProviderError, open_code_models};

pub const MAX_CATALOG_BODY_BYTES: usize = 256 * 1024;
const MAX_CATALOG_MODELS: usize = 256;
const MAX_CATALOG_MODEL_ID_BYTES: usize = 128;
const MAX_CATALOG_OWNER_BYTES: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenCodeModelAvailability {
    pub model: &'static OpenCodeModel,
    pub available: bool,
}

pub(super) fn parse_catalog(
    service: OpenCodeService,
    body: &[u8],
) -> Result<Vec<OpenCodeModelAvailability>, ProviderError> {
    if body.len() > MAX_CATALOG_BODY_BYTES {
        return Err(ProviderError::ResponseLimitExceeded);
    }
    let catalog: WireCatalog =
        serde_json::from_slice(body).map_err(|_| ProviderError::MalformedCatalog)?;
    if catalog.object != "list" || catalog.data.len() > MAX_CATALOG_MODELS {
        return Err(ProviderError::MalformedCatalog);
    }
    let mut identifiers = BTreeSet::new();
    for entry in catalog.data {
        if entry.object != "model"
            || !valid_catalog_identifier(&entry.id)
            || entry.owned_by.is_empty()
            || entry.owned_by.len() > MAX_CATALOG_OWNER_BYTES
            || entry
                .owned_by
                .bytes()
                .any(|byte| !(0x21..=0x7e).contains(&byte))
            || !identifiers.insert(entry.id)
        {
            return Err(ProviderError::MalformedCatalog);
        }
        let _ = entry.created;
    }
    Ok(open_code_models()
        .iter()
        .filter(|model| model.service == service)
        .map(|model| OpenCodeModelAvailability {
            model,
            available: identifiers.contains(model.id),
        })
        .collect())
}

fn valid_catalog_identifier(identifier: &str) -> bool {
    !identifier.is_empty()
        && identifier.len() <= MAX_CATALOG_MODEL_ID_BYTES
        && identifier.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireCatalog {
    object: String,
    data: Vec<WireCatalogEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireCatalogEntry {
    id: String,
    object: String,
    created: u64,
    owned_by: String,
}

#[cfg(test)]
mod tests {
    use super::parse_catalog;
    use crate::provider::{OpenCodeService, ProviderError};

    #[test]
    fn remote_catalog_can_only_narrow_the_reviewed_manifest() {
        let body = br#"{
            "object":"list",
            "data":[
                {"id":"gpt-5.6-luna","object":"model","created":1,"owned_by":"opencode"},
                {"id":"muse-spark-1.2-contributor","object":"model","created":1,"owned_by":"opencode"},
                {"id":"unreviewed-model","object":"model","created":1,"owned_by":"opencode"}
            ]
        }"#;
        let models = parse_catalog(OpenCodeService::Go, body).expect("catalog should decode");
        assert!(
            models
                .iter()
                .find(|entry| entry.model.id == "gpt-5.6-luna")
                .expect("reviewed model should be listed")
                .available
        );
        assert!(
            !models
                .iter()
                .find(|entry| entry.model.id == "grok-4.6")
                .expect("reviewed model should be listed")
                .available
        );
        assert!(
            models
                .iter()
                .find(|entry| entry.model.id == "muse-spark-1.2-contributor")
                .expect("reviewed contributor model should be listed")
                .available
        );
        assert!(
            models
                .iter()
                .all(|entry| entry.model.id != "unreviewed-model")
        );
    }

    #[test]
    fn catalog_rejects_duplicates_unknown_fields_and_invalid_identifiers() {
        let duplicate = br#"{"object":"list","data":[
            {"id":"grok-4.6","object":"model","created":1,"owned_by":"opencode"},
            {"id":"grok-4.6","object":"model","created":1,"owned_by":"opencode"}
        ]}"#;
        assert_eq!(
            parse_catalog(OpenCodeService::Go, duplicate),
            Err(ProviderError::MalformedCatalog)
        );

        let unknown = br#"{"object":"list","data":[],"next":"unsafe"}"#;
        assert_eq!(
            parse_catalog(OpenCodeService::Go, unknown),
            Err(ProviderError::MalformedCatalog)
        );

        let invalid = br#"{"object":"list","data":[
            {"id":"../grok-4.6","object":"model","created":1,"owned_by":"opencode"}
        ]}"#;
        assert_eq!(
            parse_catalog(OpenCodeService::Go, invalid),
            Err(ProviderError::MalformedCatalog)
        );
    }
}
