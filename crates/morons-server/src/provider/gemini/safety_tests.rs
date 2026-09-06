use super::GeminiDecoder;
use crate::provider::ProviderError;

fn event(ratings: serde_json::Value) -> Vec<u8> {
    let body = serde_json::json!({
        "candidates":[{"index":0,"content":{"role":"model","parts":[{"functionCall":{"name":"read","args":{"path":"sample.txt"}}}]},"finishReason":"STOP","safetyRatings":ratings}],
        "usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1,"totalTokenCount":2},"responseId":"safety_test"
    });
    format!("data: {body}\n\n").into_bytes()
}

#[test]
fn reviewed_safety_ratings_accept_unblocked_and_reject_blocked_tools() {
    for blocked in [false, true] {
        let mut decoder = GeminiDecoder::new(96_000, 32_000);
        let result = decoder.push(&event(serde_json::json!([{"category":"HARM_CATEGORY_DANGEROUS_CONTENT","probability":"HIGH","blocked":blocked}])));
        if blocked {
            assert_eq!(result, Err(ProviderError::ProviderExecutionFailed));
        } else {
            result.expect("unblocked rating should decode");
            decoder.finish().expect("unblocked tools should finish");
        }
    }
    let mut decoder = GeminiDecoder::new(96_000, 32_000);
    assert_eq!(decoder.push(b"data: {\"promptFeedback\":{\"safetyRatings\":[{\"category\":\"HARM_CATEGORY_HARASSMENT\",\"probability\":\"HIGH\",\"blocked\":true}]},\"responseId\":\"blocked_prompt\"}\n\n"), Err(ProviderError::ProviderExecutionFailed));
}

#[test]
fn safety_metadata_is_closed_typed_and_nonduplicated() {
    let good = serde_json::json!({"category":"HARM_CATEGORY_HARASSMENT","probability":"LOW"});
    let invalid = [
        serde_json::json!([{"arbitrary_unreviewed_field":{"anything":true}}]),
        serde_json::json!([{"category":"unreviewed","probability":"LOW"}]),
        serde_json::json!([{"category":"HARM_CATEGORY_HARASSMENT","probability":"unreviewed"}]),
        serde_json::json!([{"category":"HARM_CATEGORY_HARASSMENT","probability":"LOW","blocked":"false"}]),
        serde_json::json!([{"category":"HARM_CATEGORY_HARASSMENT","probability":"LOW","probabilityScore":0.1}]),
        serde_json::json!([good.clone(), good]),
    ];
    for ratings in invalid {
        assert_eq!(
            GeminiDecoder::new(96_000, 32_000).push(&event(ratings)),
            Err(ProviderError::MalformedResponse)
        );
    }
}
