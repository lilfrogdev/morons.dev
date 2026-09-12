use super::*;
use crate::debug_log::DebugNormalizationStage as Stage;

fn normalization_outcome(output: Vec<ProviderOutputItem>) -> ProviderOutcome {
    ProviderOutcome {
        provider_response_id: "response".to_owned(),
        output,
        usage: ProviderUsage {
            input_tokens: 1,
            cached_input_tokens: 0,
            cache_write_input_tokens: 0,
            output_tokens: 1,
            reasoning_output_tokens: 0,
            total_tokens: 2,
        },
    }
}

fn normalization_message(text: &str) -> ProviderOutputItem {
    ProviderOutputItem::AssistantMessage(ProviderAssistantMessage {
        provider_item_id: "message".to_owned(),
        phase: Some(ProviderMessagePhase::FinalAnswer),
        text: text.to_owned(),
        refusal: false,
    })
}

fn normalization_call(name: &str, arguments: &str) -> ProviderOutputItem {
    ProviderOutputItem::ToolCall(ProviderToolCall {
        provider_item_id: Some("item".to_owned()),
        provider_call_id: "call".to_owned(),
        name: name.to_owned(),
        arguments: arguments.to_owned(),
        opaque_continuation: None,
    })
}

#[test]
fn diagnosed_final_stages_preserve_error_order_and_resource_results() {
    let oversized = "x".repeat(MAX_TRANSCRIPT_TEXT_BYTES + 1);
    let cases = [
        (
            vec![],
            Stage::FinalMessageMissing,
            RunFailureKind::InvalidProviderOutput,
        ),
        (
            vec![normalization_message("")],
            Stage::FinalMessageEmpty,
            RunFailureKind::InvalidProviderOutput,
        ),
        (
            vec![normalization_message(&oversized)],
            Stage::OutputBytes,
            RunFailureKind::ResourceLimit,
        ),
        (
            vec![normalization_message(""), normalization_message("done")],
            Stage::FinalMessageMultiple,
            RunFailureKind::InvalidProviderOutput,
        ),
        (
            vec![
                normalization_message(&oversized),
                normalization_call("unknown", "{"),
            ],
            Stage::ToolInFinal,
            RunFailureKind::InvalidProviderOutput,
        ),
        (
            vec![
                normalization_message(""),
                normalization_message("done"),
                normalization_call("unknown", "{"),
            ],
            Stage::FinalMessageMultiple,
            RunFailureKind::InvalidProviderOutput,
        ),
    ];
    for (output, expected_stage, expected_failure) in cases {
        let mut stage = Stage::Other;
        let result = super::super::completed_assistant_diagnosed(
            normalization_outcome(output.clone()),
            &mut stage,
        );
        assert_eq!(result.expect_err("must reject"), expected_failure);
        assert_eq!(stage, expected_stage);
        assert_eq!(
            completed_assistant(normalization_outcome(output))
                .expect_err("silent wrapper must reject"),
            expected_failure
        );
    }
    let mut stage = Stage::Other;
    let result = super::super::completed_assistant_diagnosed(
        normalization_outcome(vec![normalization_message(
            &"x".repeat(MAX_TRANSCRIPT_TEXT_BYTES),
        )]),
        &mut stage,
    )
    .expect("exact byte limit is accepted");
    assert_eq!(result.text.len(), MAX_TRANSCRIPT_TEXT_BYTES);
    assert_eq!(stage, Stage::Other);
}

#[test]
fn diagnosed_root_preserves_catalog_boundary_and_parser_stages() {
    let invalid_catalog = crate::tools::TOOL_CATALOG_VERSION.wrapping_add(1);
    let mut stage = Stage::Other;
    assert!(matches!(
        super::super::normalize_provider_turn_diagnosed(
            normalization_outcome(vec![normalization_message("done")]),
            invalid_catalog,
            &mut stage
        ),
        Ok(NormalizedTurn::Final(_))
    ));
    assert_eq!(stage, Stage::Other);
    let cases = [
        (
            vec![normalization_call("unknown", "{")],
            invalid_catalog,
            Stage::Catalog,
        ),
        (
            vec![
                normalization_message("done"),
                normalization_call("unknown", "{"),
            ],
            invalid_catalog,
            Stage::ToolTurnMessage,
        ),
        (
            vec![normalization_call("unknown", "{")],
            crate::tools::TOOL_CATALOG_VERSION,
            Stage::ArgumentJson,
        ),
        (
            vec![normalization_call("unknown", "{}")],
            crate::tools::TOOL_CATALOG_VERSION,
            Stage::UnknownTool,
        ),
        (
            vec![normalization_call(
                "read",
                r#"{"path":"note.txt","offset":0,"limit":1}"#,
            )],
            crate::tools::TOOL_CATALOG_VERSION,
            Stage::ReadWindow,
        ),
    ];
    for (output, catalog, expected_stage) in cases {
        let mut stage = Stage::Other;
        assert!(matches!(
            super::super::normalize_provider_turn_diagnosed(
                normalization_outcome(output),
                catalog,
                &mut stage
            ),
            Err(RunFailureKind::InvalidProviderOutput)
        ));
        assert_eq!(stage, expected_stage);
    }
    let mut stage = Stage::Other;
    assert!(matches!(
        super::super::normalize_provider_turn_diagnosed(
            normalization_outcome(vec![normalization_message(
                &"x".repeat(MAX_TRANSCRIPT_TEXT_BYTES + 1)
            )]),
            invalid_catalog,
            &mut stage
        ),
        Err(RunFailureKind::ResourceLimit)
    ));
    assert_eq!(stage, Stage::OutputBytes);
}

#[test]
fn diagnosed_child_propagates_forbidden_tool_and_parser_stages() {
    for (name, arguments, expected_stage) in [
        ("ipython", r#"{"cell":"1"}"#, Stage::ForbiddenChildTool),
        ("ipython", "{", Stage::ArgumentJson),
        ("unknown", "{}", Stage::UnknownTool),
    ] {
        let mut stage = Stage::Other;
        assert!(matches!(
            super::super::normalize_subagent_provider_turn(
                normalization_outcome(vec![normalization_call(name, arguments)]),
                &mut stage
            ),
            Err(RunFailureKind::InvalidProviderOutput)
        ));
        assert_eq!(stage, expected_stage);
    }
}

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
