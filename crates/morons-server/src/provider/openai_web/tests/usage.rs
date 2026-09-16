use super::*;
use crate::{debug_log::DebugUsageRejection as Reason, web_diagnostic::WebStage};

#[test]
fn usage_rejections_are_precise_and_preserve_fail_closed_errors() {
    type Mutation = fn(&mut Value);
    let cases: &[(Reason, Mutation)] = &[
        (Reason::Missing, |r| {
            r.as_object_mut().unwrap().remove("usage");
        }),
        (Reason::Schema, |r| r["usage"] = Value::Null),
        (Reason::Schema, |r| {
            r["usage"]["input_tokens"] = json!("PRIVATE")
        }),
        (Reason::Schema, |r| r["usage"]["input_tokens"] = json!(-1)),
        (Reason::Schema, |r| r["usage"]["output_tokens"] = json!(1.5)),
        (Reason::Schema, |r| {
            r["usage"]["input_tokens_details"] = Value::Null
        }),
        (Reason::Schema, |r| {
            r["usage"]
                .as_object_mut()
                .unwrap()
                .remove("output_tokens_details");
        }),
        (Reason::InputLimit, |r| {
            r["usage"]["input_tokens"] = json!(96_001)
        }),
        (Reason::InputLimit, |r| {
            r["usage"]["input_tokens"] = json!(u64::MAX)
        }),
        (Reason::OutputLimit, |r| {
            r["usage"]["output_tokens"] = json!(32_001)
        }),
        (Reason::TotalLimit, |r| {
            r["usage"]["total_tokens"] = json!(10_000_001)
        }),
        (Reason::CachedInput, |r| {
            r["usage"]["input_tokens_details"]["cached_tokens"] = json!(13)
        }),
        (Reason::CacheWriteInput, |r| {
            r["usage"]["input_tokens_details"]["cache_write_tokens"] = json!(13)
        }),
        (Reason::ReasoningOutput, |r| {
            r["usage"]["output_tokens_details"]["reasoning_tokens"] = json!(5)
        }),
        (Reason::TotalMismatch, |r| {
            r["usage"]["total_tokens"] = json!(17)
        }),
    ];
    for (expected, mutate) in cases {
        let mut v = events();
        mutate(&mut v[3]["response"]);
        let mut stage = WebStage::Admission;
        let mut reason = None;
        let body = wire(&v);
        assert_eq!(
            super::super::decode::decode_response_detailed(&body, &mut stage, &mut reason),
            Err(ProviderError::MalformedResponse)
        );
        assert_eq!(
            decode_response(&body),
            Err(ProviderError::MalformedResponse)
        );
        assert_eq!(stage, WebStage::Usage);
        assert_eq!(reason, Some(*expected));
        let event = crate::debug_log::DebugEvent::WebUsage {
            reason: reason.unwrap(),
        };
        let encoded = serde_json::to_string(&event).unwrap();
        assert!(!encoded.contains("PRIVATE"));
        assert!(encoded.len() < 100);
    }
}

#[test]
fn usage_boundaries_remain_accepted_and_diagnostic_is_cleared() {
    for (input, output) in [(0, 0), (96_000, 32_000)] {
        let mut v = events();
        v[3]["response"]["usage"] = json!({
            "input_tokens":input, "output_tokens":output, "total_tokens":input+output,
            "input_tokens_details":{"cached_tokens":input,"cache_write_tokens":input},
            "output_tokens_details":{"reasoning_tokens":output}
        });
        let mut stage = WebStage::Admission;
        let mut reason = Some(Reason::Missing);
        let result =
            super::super::decode::decode_response_detailed(&wire(&v), &mut stage, &mut reason)
                .unwrap();
        assert_eq!(result.usage.total_tokens, input + output);
        assert_eq!(reason, None);
    }
}
