use super::*;
use serde_json::json;

fn decoder() -> ChatCompletionsDecoder {
    ChatCompletionsDecoder::new("glm-5.3-flash", 96_000, 32_000)
}

fn chunk(reason: &str) -> Value {
    json!({
        "id": "diagnostic_fixture", "object": "chat.completion.chunk",
        "created": 1, "model": "glm-5.3-flash",
        "choices": [{"index": 0, "delta": {"role": "assistant", "content": "ok"},
            "finish_reason": reason}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}
    })
}

fn push(
    decoder: &mut ChatCompletionsDecoder,
    value: &Value,
) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
    decoder.push(format!("data: {value}\n\n").as_bytes())
}

fn snapshot(
    stage: DebugStage,
    finish: DebugFinish,
    done: bool,
    usage_seen: bool,
) -> ChatDiagnosticSnapshot {
    ChatDiagnosticSnapshot {
        stage,
        finish,
        done,
        usage_seen,
    }
}

fn assert_finish_error(
    decoder: ChatCompletionsDecoder,
    error: ProviderError,
    expected: ChatDiagnosticSnapshot,
) {
    let (result, diagnostic) = decoder.finish_diagnosed();
    assert_eq!(result.err(), Some(error));
    assert_eq!(diagnostic, expected);
}

#[test]
fn successful_stop_reaches_complete_only_after_finish() {
    let mut decoder = decoder();
    assert_eq!(
        decoder.diagnostic_snapshot(),
        snapshot(DebugStage::SseFraming, DebugFinish::Absent, false, false)
    );
    let events = push(&mut decoder, &chunk("stop")).unwrap();
    assert!(matches!(&events[..], [ProviderStreamEvent::TextDelta { delta, .. }] if delta == "ok"));
    assert_eq!(
        decoder.diagnostic_snapshot(),
        snapshot(DebugStage::Usage, DebugFinish::Stop, false, true)
    );
    assert!(decoder.push(b"data: [DONE]\n\n").unwrap().is_empty());
    assert_eq!(
        decoder.diagnostic_snapshot(),
        snapshot(DebugStage::Termination, DebugFinish::Stop, true, true)
    );
    let (result, diagnostic) = decoder.finish_diagnosed();
    let outcome = result.unwrap();
    assert_eq!(outcome.provider_response_id, "diagnostic_fixture");
    assert!(
        matches!(&outcome.output[..], [ProviderOutputItem::AssistantMessage(message)]
        if message.text == "ok" && message.phase == Some(ProviderMessagePhase::FinalAnswer) && !message.refusal)
    );
    assert_eq!(outcome.usage.input_tokens, 10);
    assert_eq!(outcome.usage.output_tokens, 2);
    assert_eq!(outcome.usage.total_tokens, 12);
    assert_eq!(
        diagnostic,
        snapshot(DebugStage::Complete, DebugFinish::Stop, true, true)
    );
}

#[test]
fn terminal_failure_reasons_preserve_original_errors() {
    for (reason, finish, error) in [
        (
            "length",
            DebugFinish::Length,
            ProviderError::IncompleteResponse,
        ),
        (
            "model_context_window_exceeded",
            DebugFinish::ContextWindowExceeded,
            ProviderError::IncompleteResponse,
        ),
        (
            "content_filter",
            DebugFinish::ContentFilter,
            ProviderError::ProviderExecutionFailed,
        ),
        (
            "sensitive",
            DebugFinish::Sensitive,
            ProviderError::ProviderExecutionFailed,
        ),
        (
            "network_error",
            DebugFinish::NetworkError,
            ProviderError::ProviderExecutionFailed,
        ),
    ] {
        let mut decoder = decoder();
        push(&mut decoder, &chunk(reason)).unwrap();
        assert_eq!(
            decoder.diagnostic_snapshot(),
            snapshot(DebugStage::Usage, finish, false, true)
        );
        decoder.push(b"data: [DONE]\n\n").unwrap();
        assert_eq!(
            decoder.diagnostic_snapshot(),
            snapshot(DebugStage::Termination, finish, true, true)
        );
        assert_finish_error(
            decoder,
            error,
            snapshot(DebugStage::FinishReason, finish, true, true),
        );
    }
}

#[test]
fn missing_done_is_incomplete_despite_valid_usage() {
    let mut decoder = decoder();
    push(&mut decoder, &chunk("stop")).unwrap();
    assert_eq!(
        decoder.diagnostic_snapshot(),
        snapshot(DebugStage::Usage, DebugFinish::Stop, false, true)
    );
    assert_finish_error(
        decoder,
        ProviderError::IncompleteResponse,
        snapshot(DebugStage::Termination, DebugFinish::Stop, false, true),
    );
}

#[test]
fn missing_or_null_usage_prevents_accepting_done() {
    for null_usage in [false, true] {
        let mut value = chunk("stop");
        if null_usage {
            value["usage"] = Value::Null;
        } else {
            value.as_object_mut().unwrap().remove("usage");
        }
        let mut decoder = decoder();
        push(&mut decoder, &value).unwrap();
        assert_eq!(
            decoder.diagnostic_snapshot(),
            snapshot(DebugStage::FinishReason, DebugFinish::Stop, false, false)
        );
        assert_eq!(
            decoder.push(b"data: [DONE]\n\n").err(),
            Some(ProviderError::IncompleteResponse)
        );
        let expected = snapshot(DebugStage::Termination, DebugFinish::Stop, false, false);
        assert_eq!(decoder.diagnostic_snapshot(), expected);
        assert_finish_error(decoder, ProviderError::IncompleteResponse, expected);
    }
}

#[test]
fn malformed_json_does_not_observe_embedded_presence_markers() {
    let mut decoder = decoder();
    assert_eq!(
        decoder
            .push(b"data: {\"usage\":{},\"marker\":\"[DONE]\",\n\n")
            .err(),
        Some(ProviderError::MalformedResponse)
    );
    assert_eq!(
        decoder.diagnostic_snapshot(),
        snapshot(DebugStage::Json, DebugFinish::Absent, false, false)
    );
    assert_finish_error(
        decoder,
        ProviderError::IncompleteResponse,
        snapshot(DebugStage::Termination, DebugFinish::Absent, false, false),
    );
}

#[test]
fn invalid_identity_observes_usage_before_validation() {
    let mut value = chunk("stop");
    value["model"] = json!("synthetic_wrong_model");
    let mut decoder = decoder();
    assert_eq!(
        push(&mut decoder, &value).err(),
        Some(ProviderError::MalformedResponse)
    );
    assert_eq!(
        decoder.diagnostic_snapshot(),
        snapshot(DebugStage::Identity, DebugFinish::Absent, false, true)
    );
    assert_finish_error(
        decoder,
        ProviderError::IncompleteResponse,
        snapshot(DebugStage::Termination, DebugFinish::Absent, false, true),
    );
}

#[test]
fn rejected_usage_is_seen_but_not_accepted() {
    for usage in [
        json!({"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 13}),
        json!({"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12, "prompt_cache_hit_tokens": 11}),
        json!({"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12, "prompt_cache_hit_tokens": 3, "prompt_cache_miss_tokens": 8}),
    ] {
        let mut value = chunk("stop");
        value["usage"] = usage;
        let mut decoder = decoder();
        assert_eq!(
            push(&mut decoder, &value).err(),
            Some(ProviderError::MalformedResponse)
        );
        assert_eq!(
            decoder.diagnostic_snapshot(),
            snapshot(DebugStage::Usage, DebugFinish::Stop, false, true)
        );
        assert_eq!(
            decoder.push(b"data: [DONE]\n\n").err(),
            Some(ProviderError::IncompleteResponse)
        );
        let expected = snapshot(DebugStage::Termination, DebugFinish::Stop, false, true);
        assert_eq!(decoder.diagnostic_snapshot(), expected);
        assert_finish_error(decoder, ProviderError::IncompleteResponse, expected);
    }
}

#[test]
fn nonnull_invalid_usage_shape_is_seen() {
    let mut value = chunk("stop");
    value["usage"] = json!("synthetic_invalid_usage");
    let mut decoder = decoder();
    assert_eq!(
        push(&mut decoder, &value).err(),
        Some(ProviderError::MalformedResponse)
    );
    assert_eq!(
        decoder.diagnostic_snapshot(),
        snapshot(DebugStage::Envelope, DebugFinish::Absent, false, true)
    );
    assert_finish_error(
        decoder,
        ProviderError::IncompleteResponse,
        snapshot(DebugStage::Termination, DebugFinish::Absent, false, true),
    );
}

#[test]
fn tool_arguments_are_validated_at_consuming_finish() {
    for arguments in [
        r#"{"path":"synthetic.txt"}"#,
        "{",
        "[]",
        r#"{"path":1,"path":2}"#,
    ] {
        let mut value = chunk("tool_calls");
        value["choices"][0]["delta"] = json!({"role": "assistant", "tool_calls": [
            {"index": 0, "id": "synthetic_call", "type": "function",
             "function": {"name": "read", "arguments": arguments}}
        ]});
        let mut decoder = decoder();
        push(&mut decoder, &value).unwrap();
        assert_eq!(
            decoder.diagnostic_snapshot(),
            snapshot(DebugStage::Usage, DebugFinish::ToolCalls, false, true)
        );
        decoder.push(b"data: [DONE]\n\n").unwrap();
        assert_eq!(
            decoder.diagnostic_snapshot(),
            snapshot(DebugStage::Termination, DebugFinish::ToolCalls, true, true)
        );
        let (result, diagnostic) = decoder.finish_diagnosed();
        if arguments == r#"{"path":"synthetic.txt"}"# {
            let outcome = result.unwrap();
            assert!(
                matches!(&outcome.output[..], [ProviderOutputItem::ToolCall(call)]
                if call.provider_call_id == "synthetic_call" && call.name == "read" && call.arguments == arguments)
            );
            assert_eq!(outcome.usage.total_tokens, 12);
            assert_eq!(
                diagnostic,
                snapshot(DebugStage::Complete, DebugFinish::ToolCalls, true, true)
            );
        } else {
            assert_eq!(result.err(), Some(ProviderError::MalformedResponse));
            assert_eq!(
                diagnostic,
                snapshot(DebugStage::ArgumentJson, DebugFinish::ToolCalls, true, true)
            );
        }
    }
}

#[test]
fn unknown_empty_and_oversized_reasons_are_closed_other() {
    const MARKER: &str = "SYNTHETIC_FINISH_MARKER";
    for reason in [MARKER.to_owned(), String::new(), MARKER.repeat(7)] {
        let mut decoder = decoder();
        let result = push(&mut decoder, &chunk(&reason));
        let rejected = reason.is_empty() || reason.len() > MAX_PROVIDER_IDENTIFIER_BYTES;
        let diagnostic = decoder.diagnostic_snapshot();
        assert_eq!(
            diagnostic,
            snapshot(
                if rejected {
                    DebugStage::FinishReason
                } else {
                    DebugStage::Usage
                },
                DebugFinish::Other,
                false,
                true
            )
        );
        assert!(!format!("{diagnostic:?}").contains(MARKER));
        assert_eq!(
            serde_json::to_string(&diagnostic.finish).unwrap(),
            "\"other\""
        );
        if rejected {
            assert_eq!(result.err(), Some(ProviderError::MalformedResponse));
            assert_finish_error(
                decoder,
                ProviderError::IncompleteResponse,
                snapshot(DebugStage::Termination, DebugFinish::Other, false, true),
            );
        } else {
            result.unwrap();
            decoder.push(b"data: [DONE]\n\n").unwrap();
            assert_eq!(
                decoder.diagnostic_snapshot(),
                snapshot(DebugStage::Termination, DebugFinish::Other, true, true)
            );
            assert_finish_error(
                decoder,
                ProviderError::MalformedResponse,
                snapshot(DebugStage::FinishReason, DebugFinish::Other, true, true),
            );
        }
    }
}
