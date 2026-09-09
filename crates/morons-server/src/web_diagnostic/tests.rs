use super::*;
use crate::tools::{ToolErrorKind, ToolKind, ToolResult, validate_canonical_result};

#[test]
fn web_diagnostic_is_closed_bounded_scoped_and_preserves_legacy_uncertain_bytes() {
    let old = br#"{"status":"error","error":"uncertain"}"#;
    let legacy: ToolResult = serde_json::from_slice(old).unwrap();
    assert_eq!(serde_json::to_vec(&legacy).unwrap(), old);
    assert_eq!(
        legacy.summary(),
        "tool failed: external effect or service usage is uncertain; nothing was retried"
    );
    let failure = WebFailure {
        stage: WebStage::ResponseModel,
        category: WebCategory::MalformedResponse,
    };
    assert!(failure.valid_for_catalog(12));
    assert!(failure.valid_for_catalog(13));
    assert!(!failure.valid_for_catalog(11));
    let result = ToolResult::error(ToolErrorKind::WebSearchUncertain(failure));
    assert!(result.is_uncertain());
    assert!(validate_canonical_result(ToolKind::WebSearch, &result));
    assert!(validate_canonical_result(ToolKind::Task, &result));
    assert!(!validate_canonical_result(ToolKind::Read, &result));
    assert!(!validate_canonical_result(ToolKind::Bash, &result));
    assert_eq!(
        serde_json::from_str::<ToolResult>(&result.provider_output().unwrap()).unwrap(),
        result
    );
    let summary = result.summary();
    assert!(summary.contains("OpenAI web search is uncertain"));
    assert!(summary.contains("stage: response-model; category: malformed-response"));
    assert!(summary.contains("nothing was retried"));
    assert!(summary.is_ascii() && summary.len() < 256 && !summary.chars().any(char::is_control));
    for stage in [
        WebStage::SearchQueries,
        WebStage::SearchSources,
        WebStage::SearchActionCount,
    ] {
        let scoped = WebFailure {
            stage,
            category: WebCategory::ResponseLimitExceeded,
        };
        assert!(!scoped.valid_for_catalog(12));
        assert!(scoped.valid_for_catalog(13));
        assert!(!scoped.valid_for_catalog(14));
        assert_eq!(
            serde_json::from_str::<WebFailure>(&serde_json::to_string(&scoped).unwrap()).unwrap(),
            scoped
        );
    }
    for bad in [
        r#"{"stage":"PRIVATE","category":"malformed_response"}"#,
        r#"{"stage":"response_model","category":"PRIVATE"}"#,
        r#"{"stage":"response_model","category":"malformed_response","body":"PRIVATE"}"#,
        r#"{"stage":{"value":"PRIVATE"},"category":"malformed_response"}"#,
        r#"{"stage":"response_model"}"#,
        r#"{"stage":"response_model","stage":"usage","category":"malformed_response"}"#,
    ] {
        assert!(serde_json::from_str::<WebFailure>(bad).is_err());
    }
}
