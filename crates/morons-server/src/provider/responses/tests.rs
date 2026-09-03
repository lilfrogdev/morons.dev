use super::ResponsesDecoder;
use crate::provider::{ProviderError, ProviderOutputItem};

fn record(event: &str, json: &str) -> Vec<u8> {
    format!("event: {event}\ndata: {json}\n\n").into_bytes()
}

#[test]
fn complete_text_and_tool_stream_is_normalized() {
    let mut decoder = ResponsesDecoder::new("gpt-5.6-luna", 96_000, 32_000);
    decoder
        .push(&record("ping", r#"{"type":"ping","cost":"0"}"#))
        .expect("pre-response ping should decode");
    let fixtures = [
        record(
            "response.created",
            r#"{"type":"response.created","sequence_number":0,"response":{"id":"resp_1","object":"response","status":"in_progress","model":"gpt-5.6-luna"}}"#,
        ),
        record(
            "response.output_text.delta",
            r#"{"type":"response.output_text.delta","sequence_number":1,"output_index":0,"content_index":0,"item_id":"msg_1","delta":"hel","logprobs":[],"obfuscation":"padding"}"#,
        ),
        record(
            "response.output_text.delta",
            r#"{"type":"response.output_text.delta","sequence_number":2,"output_index":0,"content_index":0,"item_id":"msg_1","delta":"lo","logprobs":[]}"#,
        ),
        record(
            "response.function_call_arguments.delta",
            r#"{"type":"response.function_call_arguments.delta","sequence_number":3,"output_index":1,"item_id":"fc_1","delta":"{\"path\":\"a\"}","obfuscation":"padding"}"#,
        ),
        record(
            "response.completed",
            r#"{"type":"response.completed","sequence_number":4,"response":{"id":"resp_1","object":"response","model":"gpt-5.6-luna","status":"completed","output":[{"id":"msg_1","type":"message","role":"assistant","status":"completed","phase":"final_answer","content":[{"type":"output_text","text":"hello","annotations":[],"logprobs":[]}]},{"id":"fc_1","type":"function_call","status":"completed","call_id":"call_1","name":"read_file","arguments":"{\"path\":\"a\"}"}],"usage":{"input_tokens":10,"input_tokens_details":{"cached_tokens":2,"cache_write_tokens":0},"output_tokens":5,"output_tokens_details":{"reasoning_tokens":1},"total_tokens":15}}}"#,
        ),
    ];
    let mut deltas = Vec::new();
    for fixture in fixtures {
        deltas.extend(decoder.push(&fixture).expect("fixture should decode"));
    }
    decoder
        .push(b"data: [DONE]\n\n")
        .expect("done marker should decode");
    decoder
        .push(&record("ping", r#"{"type":"ping","cost":"0"}"#))
        .expect("post-response ping should decode");
    assert_eq!(deltas.len(), 2);
    let outcome = decoder.finish().expect("stream should complete");
    assert_eq!(outcome.provider_response_id, "resp_1");
    assert_eq!(outcome.output.len(), 2);
    assert!(matches!(
        outcome.output[0],
        ProviderOutputItem::AssistantMessage(_)
    ));
    assert!(matches!(outcome.output[1], ProviderOutputItem::ToolCall(_)));
    assert_eq!(outcome.usage.total_tokens, 15);
    assert!(!format!("{outcome:?}").contains("hello"));
    assert!(!format!("{outcome:?}").contains("path"));
}

#[test]
fn refusal_delta_accepts_stream_obfuscation() {
    let mut decoder = ResponsesDecoder::new("gpt-5.6-luna", 96_000, 32_000);
    decoder
        .push(&record(
            "response.created",
            r#"{"type":"response.created","sequence_number":0,"response":{"id":"resp_1","object":"response","status":"in_progress","model":"gpt-5.6-luna"}}"#,
        ))
        .expect("created event should decode");
    let deltas = decoder
        .push(&record(
            "response.refusal.delta",
            r#"{"type":"response.refusal.delta","sequence_number":1,"output_index":0,"content_index":0,"item_id":"msg_1","delta":"no","obfuscation":"padding"}"#,
        ))
        .expect("refusal delta should decode");
    assert!(matches!(
        deltas.as_slice(),
        [crate::provider::ProviderStreamEvent::TextDelta { refusal: true, .. }]
    ));
    decoder
        .push(&record(
            "response.completed",
            r#"{"type":"response.completed","sequence_number":2,"response":{"id":"resp_1","object":"response","model":"gpt-5.6-luna","status":"completed","output":[{"id":"msg_1","type":"message","role":"assistant","status":"completed","content":[{"type":"refusal","refusal":"no"}]}],"usage":{"input_tokens":1,"input_tokens_details":{"cached_tokens":0},"output_tokens":1,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":2}}}"#,
        ))
        .expect("completed refusal should decode");
    decoder.finish().expect("refusal stream should complete");
}

#[test]
fn completed_reasoning_is_bounded_and_redacted() {
    let mut decoder = ResponsesDecoder::new("gpt-5.6-sol", 96_000, 32_000);
    decoder
        .push(&record(
            "response.created",
            r#"{"type":"response.created","sequence_number":0,"response":{"id":"resp_1","object":"response","status":"in_progress","model":"gpt-5.6-sol"}}"#,
        ))
        .expect("created event should decode");
    decoder
        .push(&record(
            "response.completed",
            r#"{"type":"response.completed","sequence_number":1,"response":{"id":"resp_1","object":"response","model":"gpt-5.6-sol","status":"completed","output":[{"id":"rs_1","type":"reasoning","summary":[{"type":"summary_text","text":"bounded summary"}],"content":[],"encrypted_content":"opaque-continuation"}],"usage":{"input_tokens":1,"input_tokens_details":{"cached_tokens":0},"output_tokens":1,"output_tokens_details":{"reasoning_tokens":1},"total_tokens":2}}}"#,
        ))
        .expect("completed event should decode");
    let outcome = decoder.finish().expect("reasoning stream should complete");
    assert!(matches!(
        outcome.output.as_slice(),
        [ProviderOutputItem::Reasoning(_)]
    ));
    let debug = format!("{outcome:?}");
    assert!(!debug.contains("bounded summary"));
    assert!(!debug.contains("opaque-continuation"));
}

#[test]
fn nonterminal_reasoning_status_in_a_completed_response_is_rejected() {
    let mut decoder = ResponsesDecoder::new("gpt-5.6-luna", 96_000, 32_000);
    decoder
        .push(&record(
            "response.created",
            r#"{"type":"response.created","sequence_number":0,"response":{"id":"resp_1","object":"response","status":"in_progress","model":"gpt-5.6-luna"}}"#,
        ))
        .expect("created event should decode");
    assert_eq!(
        decoder
            .push(&record(
                "response.completed",
                r#"{"type":"response.completed","sequence_number":1,"response":{"id":"resp_1","object":"response","model":"gpt-5.6-luna","status":"completed","output":[{"id":"rs_1","type":"reasoning","status":"in_progress","summary":[],"content":[],"encrypted_content":"opaque-continuation"}],"usage":{"input_tokens":1,"input_tokens_details":{"cached_tokens":0},"output_tokens":1,"output_tokens_details":{"reasoning_tokens":1},"total_tokens":2}}}"#,
            ))
            .err(),
        Some(ProviderError::MalformedResponse)
    );
}

#[test]
fn malformed_order_contradictory_output_and_post_terminal_events_fail() {
    let mut before_created = ResponsesDecoder::new("gpt-5.6-luna", 96_000, 32_000);
    assert_eq!(
        before_created
            .push(&record(
                "response.output_text.done",
                r#"{"type":"response.output_text.done","sequence_number":0}"#,
            ))
            .err(),
        Some(ProviderError::MalformedResponse)
    );

    let mut contradiction = ResponsesDecoder::new("gpt-5.6-luna", 96_000, 32_000);
    contradiction
        .push(&record(
            "response.created",
            r#"{"type":"response.created","sequence_number":0,"response":{"id":"resp_1","object":"response","status":"in_progress","model":"gpt-5.6-luna"}}"#,
        ))
        .expect("created event should decode");
    contradiction
        .push(&record(
            "response.output_text.delta",
            r#"{"type":"response.output_text.delta","sequence_number":1,"output_index":0,"content_index":0,"item_id":"msg_1","delta":"first","logprobs":[]}"#,
        ))
        .expect("delta should decode");
    assert_eq!(
        contradiction
            .push(&record(
                "response.completed",
                r#"{"type":"response.completed","sequence_number":2,"response":{"id":"resp_1","object":"response","model":"gpt-5.6-luna","status":"completed","output":[{"id":"msg_1","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"different","annotations":[]}]}],"usage":{"input_tokens":1,"input_tokens_details":{"cached_tokens":0},"output_tokens":1,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":2}}}"#,
            ))
            .err(),
        Some(ProviderError::MalformedResponse)
    );

    let mut after_terminal = ResponsesDecoder::new("gpt-5.6-luna", 96_000, 32_000);
    after_terminal
        .push(&record(
            "response.created",
            r#"{"type":"response.created","sequence_number":0,"response":{"id":"resp_1","object":"response","status":"in_progress"}}"#,
        ))
        .expect("created event should decode");
    after_terminal
        .push(&record(
            "response.failed",
            r#"{"type":"response.failed","sequence_number":1,"response":{"id":"resp_1","object":"response","status":"failed"}}"#,
        ))
        .expect("failure event should decode");
    assert_eq!(
        after_terminal
            .push(&record(
                "response.output_text.done",
                r#"{"type":"response.output_text.done","sequence_number":2}"#,
            ))
            .err(),
        Some(ProviderError::MalformedResponse)
    );
}

#[test]
fn unknown_duplicate_truncated_and_oversized_events_fail_closed() {
    let mut unknown = ResponsesDecoder::new("gpt-5.6-luna", 96_000, 32_000);
    unknown
        .push(&record(
            "response.created",
            r#"{"type":"response.created","sequence_number":0,"response":{"id":"resp_1","object":"response","status":"in_progress"}}"#,
        ))
        .expect("created event should decode");
    assert_eq!(
        unknown
            .push(&record(
                "response.new_event",
                r#"{"type":"response.new_event","sequence_number":1}"#,
            ))
            .err(),
        Some(ProviderError::MalformedResponse)
    );

    let mut malformed_ping = ResponsesDecoder::new("gpt-5.6-luna", 96_000, 32_000);
    assert_eq!(
        malformed_ping
            .push(&record(
                "ping",
                r#"{"type":"ping","cost":"0","unexpected":true}"#,
            ))
            .err(),
        Some(ProviderError::MalformedResponse)
    );

    let mut duplicate = ResponsesDecoder::new("gpt-5.6-luna", 96_000, 32_000);
    assert_eq!(
        duplicate
            .push(&record(
                "response.created",
                r#"{"type":"response.created","type":"response.created","sequence_number":0,"response":{}}"#,
            ))
            .err(),
        Some(ProviderError::MalformedResponse)
    );

    let mut truncated = ResponsesDecoder::new("gpt-5.6-luna", 96_000, 32_000);
    truncated
        .push(&record(
            "response.created",
            r#"{"type":"response.created","sequence_number":0,"response":{"id":"resp_1","object":"response","status":"in_progress"}}"#,
        ))
        .expect("created event should decode");
    assert_eq!(truncated.finish(), Err(ProviderError::IncompleteResponse));

    let mut oversized = ResponsesDecoder::new("gpt-5.6-luna", 96_000, 32_000);
    let huge = "x".repeat(super::MAX_DELTA_BYTES + 1);
    let event = record(
        "response.output_text.delta",
        &format!(
            r#"{{"type":"response.output_text.delta","sequence_number":0,"output_index":0,"content_index":0,"item_id":"msg_1","delta":"{huge}","logprobs":[]}}"#
        ),
    );
    assert!(matches!(
        oversized.push(&event),
        Err(ProviderError::MalformedResponse | ProviderError::ResponseLimitExceeded)
    ));
}
