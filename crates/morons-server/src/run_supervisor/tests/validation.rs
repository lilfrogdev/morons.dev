use super::*;

#[test]
fn tool_turn_validation_accepts_unphased_commentary_and_rejects_invalid_output() {
    let usage = ProviderUsage {
        input_tokens: 1,
        cached_input_tokens: 0,
        cache_write_input_tokens: 0,
        output_tokens: 1,
        reasoning_output_tokens: 0,
        total_tokens: 2,
    };
    let unknown = ProviderOutcome {
        provider_response_id: "resp_unknown_tool".to_owned(),
        output: vec![ProviderOutputItem::ToolCall(ProviderToolCall {
            provider_item_id: Some("fc_unknown".to_owned()),
            provider_call_id: "call_unknown".to_owned(),
            name: "unknown_tool".to_owned(),
            arguments: "{}".to_owned(),
            opaque_continuation: None,
        })],
        usage,
    };
    assert!(matches!(
        super::normalize_provider_turn(unknown, crate::tools::TOOL_CATALOG_VERSION),
        Err(RunFailureKind::InvalidProviderOutput)
    ));

    let unphased_commentary = ProviderOutcome {
        provider_response_id: "resp_unphased_commentary".to_owned(),
        output: vec![
            ProviderOutputItem::AssistantMessage(ProviderAssistantMessage {
                provider_item_id: "msg_commentary".to_owned(),
                phase: None,
                text: "I will read the file.".to_owned(),
                refusal: false,
            }),
            ProviderOutputItem::ToolCall(ProviderToolCall {
                provider_item_id: Some("fc_read".to_owned()),
                provider_call_id: "call_read".to_owned(),
                name: "read".to_owned(),
                arguments: r#"{"path":"note.txt"}"#.to_owned(),
                opaque_continuation: None,
            }),
        ],
        usage,
    };
    let normalized =
        super::normalize_provider_turn(unphased_commentary, crate::tools::TOOL_CATALOG_VERSION)
            .expect("unphased text before a tool call should be treated as commentary");
    let super::NormalizedTurn::Tools { turn, .. } = normalized else {
        panic!("tool output should normalize as a tool turn");
    };
    assert!(matches!(
        turn.commentary,
        Some((ref text, false)) if text == "I will read the file."
    ));
    assert_eq!(turn.calls.len(), 1);

    let contradictory = ProviderOutcome {
        provider_response_id: "resp_contradictory".to_owned(),
        output: vec![
            ProviderOutputItem::AssistantMessage(ProviderAssistantMessage {
                provider_item_id: "msg_final".to_owned(),
                phase: Some(ProviderMessagePhase::FinalAnswer),
                text: "done".to_owned(),
                refusal: false,
            }),
            ProviderOutputItem::ToolCall(ProviderToolCall {
                provider_item_id: Some("fc_read".to_owned()),
                provider_call_id: "call_read".to_owned(),
                name: "read_file".to_owned(),
                arguments: r#"{"path":"note.txt","start_line":1,"line_count":1}"#.to_owned(),
                opaque_continuation: None,
            }),
        ],
        usage,
    };
    assert!(matches!(
        super::normalize_provider_turn(contradictory, crate::tools::TOOL_CATALOG_VERSION),
        Err(RunFailureKind::InvalidProviderOutput)
    ));
}

#[test]
fn nonvision_models_reject_read_image_results_before_persistence() {
    let image =
        morons_image::normalize_rgba(1, 1, vec![1, 2, 3, 255]).expect("fixture should normalize");
    let result = crate::tools::ToolResult::Ok {
        output: crate::tools::ToolOutput::ReadImage {
            path: crate::tools::ToolPath::parse("picture.png").expect("path should parse"),
            image: crate::tools::ToolImageOutput {
                attachment_id: None,
                display_name: "picture.png".to_owned(),
                media_type: image.media_type,
                width: image.width,
                height: image.height,
                bytes: image.bytes.len() as u64,
                sha256: "00".repeat(32),
                data: image.bytes,
            },
        },
    };
    assert_eq!(
        super::enforce_image_capability(result, false),
        crate::tools::ToolResult::error(crate::tools::ToolErrorKind::ImageInputUnsupported)
    );
}

#[test]
fn oversized_complete_assistant_is_a_run_resource_failure() {
    let outcome = ProviderOutcome {
        provider_response_id: "resp_oversized".to_owned(),
        output: vec![ProviderOutputItem::AssistantMessage(
            ProviderAssistantMessage {
                provider_item_id: "msg_oversized".to_owned(),
                phase: None,
                text: "x".repeat(MAX_TRANSCRIPT_TEXT_BYTES + 1),
                refusal: false,
            },
        )],
        usage: ProviderUsage {
            input_tokens: 1,
            cached_input_tokens: 0,
            cache_write_input_tokens: 0,
            output_tokens: 1,
            reasoning_output_tokens: 0,
            total_tokens: 2,
        },
    };
    assert_eq!(
        completed_assistant(outcome).expect_err("oversized assistant should fail"),
        RunFailureKind::ResourceLimit
    );
}
