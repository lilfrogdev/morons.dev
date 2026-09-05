use super::*;

fn record(data: &str) -> String {
    format!("data: {data}\n\n")
}

fn decoder() -> ChatCompletionsDecoder {
    ChatCompletionsDecoder::new("glm-5.3-flash", 96_000, 32_000)
}

#[test]
fn invalid_cache_counts_return_error_without_panicking() {
    for (prompt, cached, missed) in [(1_u64, 2_u64, 0_u64), (1, u64::MAX, 0), (2, 1, 2)] {
        let stream = record(&serde_json::json!({
            "id":"cache_error", "object":"chat.completion.chunk", "created":1, "model":"glm-5.3-flash",
            "choices":[{"index":0,"delta":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":prompt,"completion_tokens":1,"total_tokens":prompt + 1,
                "prompt_cache_hit_tokens":cached,"prompt_cache_miss_tokens":missed}
        }).to_string());
        assert_eq!(
            decoder().push(stream.as_bytes()).err(),
            Some(ProviderError::MalformedResponse)
        );
    }
}

#[test]
fn complete_text_stream_is_normalized_with_usage() {
    let mut decoder = decoder();
    let stream = [
        record(
            r#"{"id":"chat_1","object":"chat.completion.chunk","created":1,"model":"glm-5.3-flash","choices":[{"index":0,"delta":{"role":"assistant","reasoning_content":"bounded thought","reasoning_details":[{"type":"reasoning.text","text":"opaque thought"}]},"finish_reason":null,"logprobs":null}],"usage":null}"#,
        ),
        record(
            r#"{"id":"chat_1","object":"chat.completion.chunk","created":1,"model":"glm-5.3-flash","choices":[{"index":0,"delta":{"content":"hel"},"finish_reason":null,"logprobs":null}],"usage":null}"#,
        ),
        record(
            r#"{"id":"chat_1","object":"chat.completion.chunk","created":1,"model":"glm-5.3-flash","choices":[{"index":0,"delta":{"content":"lo"},"finish_reason":"stop","logprobs":null}],"usage":null}"#,
        ),
        record(
            r#"{"id":"chat_1","object":"chat.completion.chunk","created":1,"model":"glm-5.3-flash","choices":[{"index":0,"delta":{"role":"assistant","content":"","reasoning_content":null},"finish_reason":"stop","logprobs":null}],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}"#,
        ),
        record(
            r#"{"id":"chat_1","object":"chat.completion.chunk","created":1,"model":"glm-5.3-flash","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":4,"total_tokens":14,"prompt_tokens_details":{"cached_tokens":3,"cache_write_tokens":2},"completion_tokens_details":{"reasoning_tokens":1},"prompt_cache_hit_tokens":3,"prompt_cache_miss_tokens":7}}"#,
        ),
        record("[DONE]"),
        record(" [DONE] "),
        record(
            r#"{"choices":[],"cost":"0.0001"}"#,
        ),
    ]
    .concat();
    let events = decoder
        .push(stream.as_bytes())
        .expect("chat stream should decode");
    assert_eq!(events.len(), 2);
    assert!(matches!(
        &events[0],
        ProviderStreamEvent::TextDelta {
            provider_sequence: 0,
            delta,
            refusal: false,
            ..
        } if delta == "hel"
    ));
    assert!(matches!(
        &events[1],
        ProviderStreamEvent::TextDelta {
            provider_sequence: 1,
            delta,
            refusal: false,
            ..
        } if delta == "lo"
    ));
    let outcome = decoder.finish().expect("chat outcome should finish");
    assert_eq!(outcome.provider_response_id, "chat_1");
    assert!(matches!(
        &outcome.output[..],
        [ProviderOutputItem::AssistantMessage(message)]
            if message.text == "hello"
                && message.phase == Some(ProviderMessagePhase::FinalAnswer)
                && !message.refusal
    ));
    assert_eq!(outcome.usage.input_tokens, 10);
    assert_eq!(outcome.usage.cached_input_tokens, 3);
    assert_eq!(outcome.usage.cache_write_input_tokens, 2);
    assert_eq!(outcome.usage.output_tokens, 4);
    assert_eq!(outcome.usage.reasoning_output_tokens, 1);
    assert_eq!(outcome.usage.total_tokens, 14);
}

#[test]
fn streamed_tool_calls_are_ordered_and_strict() {
    let mut decoder = decoder();
    let stream = [
        record(
            r#"{"id":"chat_tools","object":"chat.completion.chunk","created":2,"model":"glm-5.3-flash","choices":[{"index":0,"delta":{"role":"assistant","content":"Checking."},"finish_reason":null,"logprobs":null}],"usage":null}"#,
        ),
        record(
            r#"{"id":"chat_tools","object":"chat.completion.chunk","created":2,"model":"glm-5.3-flash","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read","arguments":"{\"path\":"}},{"index":1,"id":"call_2","type":"function","function":{"name":"bash","arguments":"{\"command\":"}}]},"finish_reason":null,"logprobs":null}],"usage":null}"#,
        ),
        record(
            r#"{"id":"chat_tools","object":"chat.completion.chunk","created":2,"model":"glm-5.3-flash","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"README.md\"}"}},{"index":1,"function":{"arguments":"\"pwd\"}"}}]},"finish_reason":"tool_calls","logprobs":null}],"usage":null}"#,
        ),
        record(
            r#"{"id":"chat_tools","object":"chat.completion.chunk","created":2,"model":"glm-5.3-flash","choices":[],"usage":{"prompt_tokens":20,"completion_tokens":8,"total_tokens":28}}"#,
        ),
        record("[DONE]"),
    ]
    .concat();
    decoder
        .push(stream.as_bytes())
        .expect("tool stream should decode");
    let outcome = decoder.finish().expect("tool outcome should finish");
    assert!(matches!(
        &outcome.output[..],
        [
            ProviderOutputItem::AssistantMessage(message),
            ProviderOutputItem::ToolCall(first),
            ProviderOutputItem::ToolCall(second),
        ] if message.phase == Some(ProviderMessagePhase::Commentary)
            && message.text == "Checking."
            && first.provider_call_id == "call_1"
            && first.name == "read"
            && first.arguments == r#"{"path":"README.md"}"#
            && second.provider_call_id == "call_2"
            && second.name == "bash"
            && second.arguments == r#"{"command":"pwd"}"#
    ));
}

#[test]
fn malformed_incomplete_and_oversized_streams_fail_closed() {
    let mut compatible = decoder();
    compatible
        .push(
            [
                record(
                    r#"{"id":"chat_compatible","request_id":"request_compatible","created":3,"model":"glm-5.3-flash","choices":[{"index":0,"delta":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2,"prompt_tokens_details":{"cached_tokens":0,"cache_write_tokens":null,"audio_tokens":null},"completion_tokens_details":{"reasoning_tokens":null,"audio_tokens":null,"accepted_prediction_tokens":null,"rejected_prediction_tokens":null}}}"#,
                ),
                record("[DONE]"),
            ]
            .concat()
            .as_bytes(),
        )
        .expect("documented GLM stream shape should decode");
    compatible
        .finish()
        .expect("documented GLM stream should finish");

    let mut invalid_trailer = decoder();
    assert_eq!(
        invalid_trailer
            .push(
                [
                    record(
                        r#"{"id":"chat_trailer","created":4,"model":"glm-5.3-flash","choices":[{"index":0,"delta":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#,
                    ),
                    record("[DONE]"),
                    record(r#"{"choices":[],"cost":"not-a-decimal"}"#),
                ]
                .concat()
                .as_bytes(),
            )
            .err(),
        Some(ProviderError::MalformedResponse)
    );

    let mut inconsistent_cache_usage = decoder();
    assert_eq!(
        inconsistent_cache_usage
            .push(
                record(
                    r#"{"id":"chat_cache","object":"chat.completion.chunk","created":4,"model":"glm-5.3-flash","choices":[{"index":0,"delta":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4,"prompt_tokens_details":{"cached_tokens":1},"prompt_cache_hit_tokens":2,"prompt_cache_miss_tokens":1}}"#,
                )
                .as_bytes(),
            )
            .err(),
        Some(ProviderError::MalformedResponse)
    );

    let mut contradictory_usage = decoder();
    assert_eq!(
        contradictory_usage
            .push(
                [
                    record(
                        r#"{"id":"chat_usage","object":"chat.completion.chunk","created":4,"model":"glm-5.3-flash","choices":[{"index":0,"delta":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}"#,
                    ),
                    record(
                        r#"{"id":"chat_usage","object":"chat.completion.chunk","created":4,"model":"glm-5.3-flash","choices":[],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#,
                    ),
                ]
                .concat()
                .as_bytes(),
            )
            .err(),
        Some(ProviderError::MalformedResponse)
    );

    let duplicate = record(
        r#"{"id":"chat_1","id":"chat_2","object":"chat.completion.chunk","created":1,"model":"glm-5.3-flash","choices":[],"usage":null}"#,
    );
    assert_eq!(
        decoder().push(duplicate.as_bytes()).err(),
        Some(ProviderError::MalformedResponse)
    );

    let mut missing_usage = decoder();
    missing_usage
        .push(
            [
                record(
                    r#"{"id":"chat_1","object":"chat.completion.chunk","created":1,"model":"glm-5.3-flash","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop","logprobs":null}],"usage":null}"#,
                ),
                record("[DONE]"),
            ]
            .concat()
            .as_bytes(),
        )
        .expect_err("done marker without usage should fail");

    let oversized = "x".repeat(MAX_DELTA_BYTES + 1);
    let body = record(&format!(
        r#"{{"id":"chat_1","object":"chat.completion.chunk","created":1,"model":"glm-5.3-flash","choices":[{{"index":0,"delta":{{"content":"{oversized}"}},"finish_reason":null,"logprobs":null}}],"usage":null}}"#
    ));
    assert_eq!(
        decoder().push(body.as_bytes()).err(),
        Some(ProviderError::ResponseLimitExceeded)
    );

    let mut wrong_finish = decoder();
    let stream = [
        record(
            r#"{"id":"chat_1","object":"chat.completion.chunk","created":1,"model":"glm-5.3-flash","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"tool_calls","logprobs":null}],"usage":null}"#,
        ),
        record(
            r#"{"id":"chat_1","object":"chat.completion.chunk","created":1,"model":"glm-5.3-flash","choices":[],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#,
        ),
        record("[DONE]"),
    ]
    .concat();
    wrong_finish
        .push(stream.as_bytes())
        .expect("records should decode before terminal validation");
    assert_eq!(wrong_finish.finish(), Err(ProviderError::MalformedResponse));
}
