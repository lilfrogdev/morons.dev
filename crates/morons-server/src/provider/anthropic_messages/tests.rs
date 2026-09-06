use super::AnthropicMessagesDecoder;
use crate::provider::{
    ProviderError, ProviderMessagePhase, ProviderOutputItem, ProviderStreamEvent,
};

fn plain_stream(stop_reason: &str) -> String {
    format!(
        concat!(
            "event: message_start\n",
            "data: {{\"type\":\"message_start\",\"message\":{{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"qwen3.8-max\",\"content\":[],\"container\":null,\"stop_details\":null,\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{{\"input_tokens\":10,\"cache_creation_input_tokens\":2,\"cache_read_input_tokens\":3,\"output_tokens\":0,\"cache_creation\":{{\"ephemeral_1h_input_tokens\":0,\"ephemeral_5m_input_tokens\":2}},\"server_tool_use\":{{\"web_fetch_requests\":0,\"web_search_requests\":0}},\"service_tier\":\"standard\",\"inference_geo\":\"us\"}}}}}}\n\n",
            "event: content_block_start\n",
            "data: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\",\"citations\":null}}}}\n\n",
            "event: content_block_delta\n",
            "data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"hel\"}}}}\n\n",
            "event: content_block_delta\n",
            "data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"lo\"}}}}\n\n",
            "event: content_block_stop\n",
            "data: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n",
            "event: message_delta\n",
            "data: {{\"type\":\"message_delta\",\"delta\":{{\"container\":null,\"stop_details\":null,\"stop_reason\":\"{}\",\"stop_sequence\":null}},\"usage\":{{\"input_tokens\":12,\"cache_creation_input_tokens\":2,\"cache_read_input_tokens\":3,\"output_tokens\":4,\"server_tool_use\":{{\"web_fetch_requests\":0,\"web_search_requests\":0}}}}}}\n\n",
            "event: message_stop\n",
            "data: {{\"type\":\"message_stop\"}}\n\n"
        ),
        stop_reason
    )
}

#[test]
fn decodes_bounded_text_stream_and_usage() {
    let stream = plain_stream("end_turn");
    let mut decoder = AnthropicMessagesDecoder::new("qwen3.8-max", 96_000, 32_000);
    let split = stream.len() / 2;
    let mut events = decoder
        .push(&stream.as_bytes()[..split])
        .expect("first half should decode");
    events.extend(
        decoder
            .push(&stream.as_bytes()[split..])
            .expect("second half should decode"),
    );
    assert_eq!(events.len(), 2);
    assert!(matches!(
        &events[0],
        ProviderStreamEvent::TextDelta {
            provider_sequence: 0,
            output_index: 0,
            content_index: 0,
            delta,
            refusal: false,
        } if delta == "hel"
    ));
    let outcome = decoder.finish().expect("stream should finish");
    assert_eq!(outcome.provider_response_id, "msg_1");
    assert_eq!(outcome.usage.input_tokens, 17);
    assert_eq!(outcome.usage.cached_input_tokens, 3);
    assert_eq!(outcome.usage.cache_write_input_tokens, 2);
    assert_eq!(outcome.usage.output_tokens, 4);
    assert_eq!(outcome.usage.total_tokens, 21);
    assert!(matches!(
        &outcome.output[0],
        ProviderOutputItem::AssistantMessage(message)
            if message.text == "hello"
                && message.phase == Some(ProviderMessagePhase::FinalAnswer)
                && !message.refusal
    ));
}

#[test]
fn decodes_tool_input_and_ignores_bounded_thinking() {
    let stream = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_tools\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"minimax-m3\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\n",
        "event: ping\n",
        "data: {\"type\":\"ping\"}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"bounded thought\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"opaque\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_1\",\"name\":\"read\",\"input\":{},\"caller\":{\"type\":\"direct\"}}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"README.md\\\"}\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\",\"stop_sequence\":null},\"usage\":{\"input_tokens\":12,\"output_tokens\":8}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\",\"cost\":\"0.01\"}\n\n"
    );
    let mut decoder = AnthropicMessagesDecoder::new("minimax-m3", 96_000, 32_000);
    assert!(
        decoder
            .push(stream.as_bytes())
            .expect("tool stream should decode")
            .is_empty()
    );
    let outcome = decoder.finish().expect("tool stream should finish");
    assert_eq!(outcome.usage.input_tokens, 12);
    assert!(matches!(
        &outcome.output[0],
        ProviderOutputItem::ToolCall(call)
            if call.provider_call_id == "call_1"
                && call.name == "read"
                && call.arguments == r#"{"path":"README.md"}"#
    ));
}

#[test]
fn rejects_event_mismatch_incomplete_stream_and_terminal_limit() {
    let mut mismatch = AnthropicMessagesDecoder::new("qwen3.8-max", 96_000, 32_000);
    assert_eq!(
        mismatch
            .push(b"event: ping\ndata: {\"type\":\"message_stop\"}\n\n")
            .err(),
        Some(ProviderError::MalformedResponse)
    );

    let mut incomplete = AnthropicMessagesDecoder::new("qwen3.8-max", 96_000, 32_000);
    let partial = plain_stream("end_turn");
    let stop = partial
        .find("event: message_delta")
        .expect("fixture should have a message delta");
    incomplete
        .push(&partial.as_bytes()[..stop])
        .expect("partial stream should decode");
    assert_eq!(incomplete.finish(), Err(ProviderError::IncompleteResponse));

    let mut limited = AnthropicMessagesDecoder::new("qwen3.8-max", 96_000, 32_000);
    limited
        .push(plain_stream("max_tokens").as_bytes())
        .expect("terminal limit stream should decode structurally");
    assert_eq!(limited.finish(), Err(ProviderError::IncompleteResponse));
}
