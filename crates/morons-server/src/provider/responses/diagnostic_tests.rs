use super::*;
use serde_json::json;
type GuardCase = (ResponseStage, fn(&mut Value));

fn record(value: Value) -> Vec<u8> {
    format!("data: {value}\n\n").into_bytes()
}
fn created() -> Value {
    json!({"type":"response.created","sequence_number":0,"response":{"id":"resp_PRIVATE","object":"response","model":"gpt-5.5","status":"in_progress"}})
}
fn completed() -> Value {
    json!({"type":"response.completed","sequence_number":1,"response":{"id":"resp_PRIVATE","object":"response","model":"gpt-5.5","status":"completed","output":[{"id":"msg_PRIVATE","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"PRIVATE","annotations":[]}]}],"usage":{"input_tokens":10,"input_tokens_details":{"cached_tokens":0},"output_tokens":5,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":15}}})
}
fn check(value: Value, active: bool, expected: ResponseStage, error: ProviderError) {
    let mut decoder = ResponsesDecoder::new("gpt-5.5", 96_000, 32_000);
    if active {
        decoder.push(&record(created())).unwrap();
    }
    assert_eq!(decoder.push(&record(value)).err(), Some(error));
    assert_eq!(decoder.failure_stage(), expected);
    assert!(!format!("{error:?} {error} {:?}", decoder.failure_stage()).contains("PRIVATE"));
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn response_guard_diagnostics_are_fixed_without_changing_rejection_categories() {
    let cases: &[GuardCase] = &[
        (ResponseStage::Sequence, |v| v["sequence_number"] = json!(9)),
        (ResponseStage::SequenceField, |v| {
            v["sequence_number"] = json!(null)
        }),
        (ResponseStage::EventEnvelope, |v| v["type"] = json!(null)),
        (ResponseStage::EventKind, |v| {
            v["type"] = json!("PRIVATE-event")
        }),
        (ResponseStage::Lifecycle, |v| {
            v["type"] = json!("response.in_progress")
        }),
        (ResponseStage::ResponseIdentity, |v| {
            v["response"]["object"] = json!("PRIVATE-object")
        }),
        (ResponseStage::ResponseModel, |v| {
            v["response"]["model"] = json!("PRIVATE-model")
        }),
    ];
    for &(stage, mutate) in cases {
        let mut value = created();
        mutate(&mut value);
        check(value, false, stage, ProviderError::MalformedResponse);
    }
    let cases: &[GuardCase] = &[
        (ResponseStage::CompletedEnvelope, |v| {
            v["response"]["output"] = json!(null)
        }),
        (ResponseStage::CompletedIdentity, |v| {
            v["response"]["id"] = json!("different_PRIVATE")
        }),
        (ResponseStage::ResponseModel, |v| {
            v["response"]["model"] = json!("PRIVATE-model")
        }),
        (ResponseStage::Usage, |v| {
            v["response"]["usage"]["total_tokens"] = json!(16)
        }),
        (ResponseStage::Usage, |v| {
            v["response"]["usage"]["input_tokens_details"] = json!(null)
        }),
        (ResponseStage::Usage, |v| {
            v["response"]["usage"] = json!("PRIVATE")
        }),
        (ResponseStage::AssistantMessage, |v| {
            v["response"]["output"][0]["role"] = json!("PRIVATE-role")
        }),
        (ResponseStage::OutputConsistency, |v| {
            v["response"]["output"][0]["type"] = json!("PRIVATE-item")
        }),
        (
            ResponseStage::Reasoning,
            |v| v["response"]["output"] = json!([{"type":"reasoning","id":"rs_PRIVATE","summary":[],"encrypted_content":"PRIVATE","content":[{"text":"PRIVATE"}]}]),
        ),
        (
            ResponseStage::FunctionCall,
            |v| v["response"]["output"] = json!([{"type":"function_call","id":"fc_PRIVATE","call_id":"call_PRIVATE","name":"read","status":"completed","arguments":"PRIVATE-invalid-json"}]),
        ),
    ];
    for &(stage, mutate) in cases {
        let mut value = completed();
        mutate(&mut value);
        check(value, true, stage, ProviderError::MalformedResponse);
    }
    for (event, stage) in [
        ("response.output_text.delta", ResponseStage::TextDelta),
        ("response.refusal.delta", ResponseStage::RefusalDelta),
        (
            "response.function_call_arguments.delta",
            ResponseStage::ArgumentDelta,
        ),
    ] {
        check(
            json!({"type":event,"sequence_number":1,"PRIVATE-field":"PRIVATE"}),
            true,
            stage,
            ProviderError::MalformedResponse,
        );
    }
    let mut oversized = created();
    for i in 0..257 {
        oversized[format!("PRIVATE-{i}")] = json!(0);
    }
    check(
        oversized,
        false,
        ResponseStage::EventBounds,
        ProviderError::ResponseLimitExceeded,
    );
    for (bytes, stage) in [
        (
            b"event: \xff\ndata: {}\n\n".as_slice(),
            ResponseStage::SseFraming,
        ),
        (b"data: {PRIVATE}\n\n".as_slice(), ResponseStage::Json),
    ] {
        let mut decoder = ResponsesDecoder::new("gpt-5.5", 96_000, 32_000);
        assert!(decoder.push(bytes).is_err());
        assert_eq!(decoder.failure_stage(), stage);
    }
    let mut decoder = ResponsesDecoder::new("gpt-5.5", 96_000, 32_000);
    decoder.push(&record(created())).unwrap();
    decoder.push(&record(completed())).unwrap();
    assert_eq!(decoder.finish().unwrap().usage.total_tokens, 15);
}
