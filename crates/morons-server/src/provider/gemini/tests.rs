use super::GeminiDecoder;
use crate::provider::{ProviderError, ProviderOutputItem, ProviderStreamEvent};

fn data(value: &str) -> Vec<u8> {
    format!("data: {value}\n\n").into_bytes()
}

#[test]
fn decodes_text_reasoning_usage_and_identity() {
    let mut decoder = GeminiDecoder::new(96_000, 32_000);
    let first = data(
        r#"{"candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"private","thought":true,"thoughtSignature":"opaque"},{"text":"Hel"}]}}],"usageMetadata":{"promptTokenCount":8},"modelVersion":"gemini-3.8-flash","responseId":"resp_1","createTime":"2026-09-04T18:07:00.123456Z"}"#,
    );
    let events = decoder.push(&first).expect("first event should decode");
    assert_eq!(events.len(), 1);
    assert!(matches!(
        &events[0],
        ProviderStreamEvent::TextDelta { delta, .. } if delta == "Hel"
    ));

    let final_event = data(
        r#"{"candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"lo"}]},"finishReason":"STOP","safetyRatings":[]}],"usageMetadata":{"promptTokenCount":8,"cachedContentTokenCount":3,"candidatesTokenCount":2,"thoughtsTokenCount":1,"totalTokenCount":11,"promptTokensDetails":[{"modality":"TEXT","tokenCount":8}]},"modelVersion":"gemini-3.8-flash","responseId":"resp_1"}"#,
    );
    let events = decoder
        .push(&final_event)
        .expect("terminal event should decode");
    assert_eq!(events.len(), 1);
    let outcome = decoder.finish().expect("stream should finish");
    assert_eq!(outcome.provider_response_id, "resp_1");
    assert_eq!(outcome.usage.input_tokens, 8);
    assert_eq!(outcome.usage.cached_input_tokens, 3);
    assert_eq!(outcome.usage.output_tokens, 3);
    assert_eq!(outcome.usage.reasoning_output_tokens, 1);
    assert_eq!(outcome.usage.total_tokens, 11);
    assert!(matches!(
        &outcome.output[0],
        ProviderOutputItem::AssistantMessage(message)
            if message.text == "Hello" && !message.refusal
    ));
}

#[test]
fn text_signatures_are_bounded_discarded_and_do_not_hide_visible_text() {
    for thought in [None, Some(false)] {
        let mut decoder = GeminiDecoder::new(96_000, 32_000);
        for text in ["visible", ""] {
            let mut part =
                serde_json::json!({"text": text, "thoughtSignature": "opaque-text-signature"});
            if let Some(thought) = thought {
                part["thought"] = thought.into();
            }
            let event = serde_json::json!({
                "candidates": [{"index": 0, "content": {"role": "model", "parts": [part]}}],
                "responseId": "resp_1"
            });
            let events = decoder.push(&data(&event.to_string())).unwrap();
            assert_eq!(events.len(), usize::from(!text.is_empty()));
            assert!(!format!("{events:?}").contains("opaque-text-signature"));
        }
        decoder.push(&data(r#"{"candidates":[{"index":0,"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1,"totalTokenCount":2},"responseId":"resp_1"}"#)).unwrap();
        let outcome = decoder.finish().unwrap();
        assert!(
            matches!(&outcome.output[..], [ProviderOutputItem::AssistantMessage(message)] if message.text == "visible")
        );
        assert!(!format!("{outcome:?}").contains("opaque-text-signature"));
    }

    let mut decoder = GeminiDecoder::new(96_000, 32_000);
    let part = serde_json::json!({"text": "", "thoughtSignature": "x".repeat(super::MAX_IGNORED_REASONING_BYTES / 2)});
    decoder.process_part(part.clone()).unwrap();
    decoder.process_part(part).unwrap();
    assert_eq!(
        decoder.process_part(serde_json::json!({"text": "", "thoughtSignature": "x"})),
        Err(ProviderError::ResponseLimitExceeded)
    );
    let mut decoder = GeminiDecoder::new(96_000, 32_000);
    assert_eq!(
        decoder.process_part(serde_json::json!({"text": "", "thoughtSignature": ""})),
        Err(ProviderError::ResponseLimitExceeded)
    );
    assert_eq!(
        decoder.process_part(
            serde_json::json!({"text": "", "thoughtSignature": "x", "unreviewed": true})
        ),
        Err(ProviderError::MalformedResponse)
    );
}

#[test]
fn decodes_function_calls_with_deterministic_local_ids() {
    let mut decoder = GeminiDecoder::new(96_000, 32_000);
    let event = data(
        r#"{"candidates":[{"index":0,"content":{"role":"model","parts":[{"functionCall":{"name":"read","args":{"path":"README.md"}},"thoughtSignature":"opaque-secret-signature"}]},"finishReason":"STOP","finishMessage":"Model generated function call(s)."}],"usageMetadata":{"promptTokenCount":4,"candidatesTokenCount":2,"totalTokenCount":6,"serviceTier":"standard"},"modelVersion":"gemini-3.8-flash","responseId":"resp_tool"}"#,
    );
    assert!(
        decoder
            .push(&event)
            .expect("event should decode")
            .is_empty()
    );
    decoder
        .push(&data(r#"{"type":"ping","cost":"0.00000001"}"#))
        .expect("Zen cost trailer should decode");
    assert_eq!(
        decoder.push(&data(r#"{"type":"ping","cost":"0.00000001"}"#)),
        Err(ProviderError::MalformedResponse)
    );
    let outcome = decoder.finish().expect("tool stream should finish");
    assert!(matches!(
        &outcome.output[0],
        ProviderOutputItem::ToolCall(call)
            if call.provider_call_id.starts_with("gemini_call_")
                && call.provider_call_id.len() == 44
                && call.name == "read"
                && call.arguments == r#"{"path":"README.md"}"#
                && call.opaque_continuation.as_deref() == Some("opaque-secret-signature")
    ));
    assert!(!format!("{:?}", outcome.output[0]).contains("opaque-secret-signature"));
}

#[test]
fn rejects_unknown_shapes_conflicting_identity_and_unrequested_grounding() {
    let mut early_ping = GeminiDecoder::new(96_000, 32_000);
    assert_eq!(
        early_ping.push(&data(r#"{"type":"ping","cost":"0"}"#)),
        Err(ProviderError::MalformedResponse)
    );

    let mut malformed_cost = GeminiDecoder::new(96_000, 32_000);
    malformed_cost
        .push(&data(
            r#"{"candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"done"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1,"totalTokenCount":2},"responseId":"resp_1"}"#,
        ))
        .expect("terminal response should decode before the cost trailer");
    assert_eq!(
        malformed_cost.push(&data(r#"{"type":"ping","cost":"not-a-decimal"}"#)),
        Err(ProviderError::MalformedResponse)
    );

    let mut unknown = GeminiDecoder::new(96_000, 32_000);
    assert_eq!(
        unknown.push(&data(r#"{"serverTool":{"name":"unsafe"}}"#)),
        Err(ProviderError::MalformedResponse)
    );

    let mut malformed_time = GeminiDecoder::new(96_000, 32_000);
    assert_eq!(
        malformed_time.push(&data(
            r#"{"candidates":[],"responseId":"resp_1","createTime":"not-a-timestamp"}"#,
        )),
        Err(ProviderError::MalformedResponse)
    );

    let mut mismatch = GeminiDecoder::new(96_000, 32_000);
    mismatch
        .push(&data(
            r#"{"candidates":[],"responseId":"resp_1","modelVersion":"gemini-3.8-flash","createTime":"2026-09-04T18:07:00Z"}"#,
        ))
        .expect("initial identity should decode");
    assert_eq!(
        mismatch.push(&data(
            r#"{"candidates":[],"responseId":"resp_1","modelVersion":"gemini-3.8-flash","createTime":"2026-09-04T18:07:01Z"}"#,
        )),
        Err(ProviderError::MalformedResponse)
    );

    let mut response_mismatch = GeminiDecoder::new(96_000, 32_000);
    response_mismatch
        .push(&data(r#"{"candidates":[],"responseId":"resp_1"}"#))
        .expect("initial response identity should decode");
    assert_eq!(
        response_mismatch.push(&data(r#"{"candidates":[],"responseId":"resp_2"}"#)),
        Err(ProviderError::MalformedResponse)
    );

    let mut grounded = GeminiDecoder::new(96_000, 32_000);
    assert_eq!(
        grounded.push(&data(
            r#"{"candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"answer"}]},"groundingMetadata":{"webSearchQueries":["query"]}}],"responseId":"resp_1"}"#,
        )),
        Err(ProviderError::MalformedResponse)
    );

    let mut duplicate_terminal = GeminiDecoder::new(96_000, 32_000);
    duplicate_terminal
        .push(&data(
            r#"{"candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"done"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1,"totalTokenCount":2},"responseId":"resp_1"}"#,
        ))
        .expect("first terminal event should decode");
    assert_eq!(
        duplicate_terminal.push(&data(
            r#"{"candidates":[{"index":0,"finishReason":"STOP"}],"responseId":"resp_1"}"#,
        )),
        Err(ProviderError::MalformedResponse)
    );
}

#[test]
fn rejects_blocked_incomplete_and_inconsistent_usage_streams() {
    let mut blocked = GeminiDecoder::new(96_000, 32_000);
    assert_eq!(
        blocked.push(&data(
            r#"{"promptFeedback":{"blockReason":"SAFETY","safetyRatings":[]},"responseId":"resp_1"}"#,
        )),
        Err(ProviderError::ProviderExecutionFailed)
    );

    let mut incomplete = GeminiDecoder::new(96_000, 32_000);
    incomplete
        .push(&data(
            r#"{"candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"partial"}]}}],"responseId":"resp_1"}"#,
        ))
        .expect("partial event should decode");
    assert!(matches!(
        incomplete.finish(),
        Err(ProviderError::IncompleteResponse)
    ));

    let mut unsupported_tier = GeminiDecoder::new(96_000, 32_000);
    assert_eq!(
        unsupported_tier.push(&data(
            r#"{"candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"done"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1,"totalTokenCount":2,"serviceTier":"priority"},"responseId":"resp_1"}"#,
        )),
        Err(ProviderError::MalformedResponse)
    );

    let mut orphaned_finish_message = GeminiDecoder::new(96_000, 32_000);
    assert_eq!(
        orphaned_finish_message.push(&data(
            r#"{"candidates":[{"index":0,"finishMessage":"not terminal"}],"responseId":"resp_1"}"#,
        )),
        Err(ProviderError::MalformedResponse)
    );

    let mut usage = GeminiDecoder::new(96_000, 32_000);
    assert_eq!(
        usage.push(&data(
            r#"{"candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"done"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":4,"cachedContentTokenCount":5,"candidatesTokenCount":1,"totalTokenCount":5},"responseId":"resp_1"}"#,
        )),
        Err(ProviderError::MalformedResponse)
    );
}
