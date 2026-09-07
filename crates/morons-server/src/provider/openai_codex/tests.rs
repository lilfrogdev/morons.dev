mod images;
mod transport;
use super::*;
use crate::provider::{
    DataUseRestrictions, ProviderError, ProviderInputItem, ProviderMessagePhase,
    ProviderMessageRole, ProviderTool,
};
use serde_json::{Value, json};
fn turn() -> CodexTurn {
    CodexTurn::new(std::sync::Arc::new(()), [1; 16], [2; 16], 1, "gpt-5.5").unwrap()
}
fn input() -> Vec<ProviderInputItem> {
    vec![ProviderInputItem::Message {
        role: ProviderMessageRole::User,
        text: "fixture prompt".into(),
        phase: None,
    }]
}
fn request(turn: &CodexTurn) -> CodexRequest {
    CodexRequest::new(
        turn,
        "Morons fixture instructions",
        input(),
        Vec::new(),
        CodexRequestLimits {
            estimated_input_tokens: 100,
            maximum_output_tokens: 32,
        },
        DataUseRestrictions::default(),
    )
    .unwrap()
}
#[test]
fn native_body_is_explicit_and_does_not_claim_remote_output_limit_enforcement() {
    let mut turn = turn();
    turn.reasoning.insert(super::turn::reasoning_fingerprint(
        "rs_fixture",
        &[],
        Some("opaque-fixture"),
    ));
    let mut input = input();
    input.insert(
        0,
        ProviderInputItem::Message {
            role: ProviderMessageRole::Developer,
            text: "untrusted guidance".into(),
            phase: None,
        },
    );
    input.push(ProviderInputItem::Message {
        role: ProviderMessageRole::Assistant,
        text: "working".into(),
        phase: Some(ProviderMessagePhase::Commentary),
    });
    input.push(ProviderInputItem::Reasoning {
        id: "rs_fixture".into(),
        summaries: Vec::new(),
        encrypted_content: Some("opaque-fixture".into()),
    });
    input.push(ProviderInputItem::FunctionCall {
        call_id: "call_1".into(),
        name: "fixture".into(),
        arguments: "{}".into(),
        opaque_continuation: None,
    });
    input.push(ProviderInputItem::FunctionCallOutput {
        call_id: "call_1".into(),
        output: "done".into(),
    });
    let tools = vec![ProviderTool {
        name: "fixture".into(),
        description: "fixture tool".into(),
        parameters: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        strict: true,
    }];
    let request = CodexRequest::new(
        &turn,
        "Morons core",
        input,
        tools,
        CodexRequestLimits {
            estimated_input_tokens: 100,
            maximum_output_tokens: 1,
        },
        DataUseRestrictions::default(),
    )
    .unwrap();
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["instructions"], "Morons core");
    assert_eq!(body["input"][0]["content"], "untrusted guidance");
    assert_eq!(body["input"][2]["phase"], "commentary");
    assert_eq!(body["input"][3]["encrypted_content"], "opaque-fixture");
    assert_eq!(body["input"][4]["type"], "function_call");
    assert_eq!(body["input"][5]["type"], "function_call_output");
    assert_eq!(body["model"], "gpt-5.5");
    assert_eq!(body["reasoning"], json!({"effort":"medium"}));
    assert_eq!(body["text"], json!({"verbosity":"low"}));
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["tools"][0]["strict"], true);
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(body["store"], false);
    assert_eq!(body["parallel_tool_calls"], false);
    assert_eq!(body["stream"], true);
    for field in [
        "max_output_tokens",
        "temperature",
        "service_tier",
        "previous_response_id",
        "client_metadata",
    ] {
        assert!(body.get(field).is_none());
    }
    assert!(!format!("{request:?} {turn:?}").contains("opaque-fixture"));
    assert!(!format!("{request:?}").contains("Morons core"));
}
#[test]
fn policy_turn_identity_and_manifest_are_checked_before_dispatch() {
    let turn = turn();
    let request = request(&turn);
    for policy in [
        DataUseRestrictions {
            block_training_use: true,
            require_zero_retention: false,
        },
        DataUseRestrictions {
            block_training_use: false,
            require_zero_retention: true,
        },
    ] {
        assert!(matches!(
            request.validate(&turn, policy),
            Err(ProviderError::DataUseRestricted)
        ));
        assert!(matches!(
            CodexRequest::new(&turn, "core", input(), Vec::new(), request.limits, policy),
            Err(ProviderError::DataUseRestricted)
        ));
    }
    let other = CodexTurn::new(std::sync::Arc::new(()), [1; 16], [2; 16], 1, "gpt-5.5").unwrap();
    assert!(
        request
            .validate(&other, DataUseRestrictions::default())
            .is_err()
    );
    for model in [
        "gpt-5.4",
        "gpt-5.4-mini",
        "gpt-5.6-luna",
        "gpt-5.5-pro",
        "https://evil.invalid/",
    ] {
        assert!(matches!(
            CodexTurn::new(std::sync::Arc::new(()), [1; 16], [2; 16], 1, model),
            Err(ProviderError::UnsupportedModel)
        ));
    }
    for (conversation, run, generation) in [
        ([0; 16], [2; 16], 1),
        ([1; 16], [0; 16], 1),
        ([1; 16], [2; 16], 0),
        ([1; 16], [2; 16], u64::MAX),
    ] {
        assert!(
            CodexTurn::new(
                std::sync::Arc::new(()),
                conversation,
                run,
                generation,
                "gpt-5.5"
            )
            .is_err()
        );
    }
}
#[test]
fn request_bounds_and_conversation_identifiers_do_not_cross_credentials_or_runs() {
    let mut turn = turn();
    let limits = CodexRequestLimits {
        estimated_input_tokens: 100,
        maximum_output_tokens: 32,
    };
    for (instructions, limits) in [
        ("".into(), limits),
        ("x".repeat(65537), limits),
        (
            "core".into(),
            CodexRequestLimits {
                estimated_input_tokens: 96001,
                ..limits
            },
        ),
        (
            "core".into(),
            CodexRequestLimits {
                maximum_output_tokens: 32001,
                ..limits
            },
        ),
    ] {
        assert!(
            CodexRequest::new(
                &turn,
                &instructions,
                input(),
                Vec::new(),
                limits,
                DataUseRestrictions::default()
            )
            .is_err()
        );
    }
    assert!(
        CodexRequest::new(
            &turn,
            "core",
            Vec::new(),
            Vec::new(),
            limits,
            DataUseRestrictions::default()
        )
        .is_err()
    );
    let changed = CodexTurn::new(std::sync::Arc::new(()), [1; 16], [2; 16], 2, "gpt-5.5").unwrap();
    assert_ne!(turn.identity.session, changed.identity.session);
    let child = CodexTurn::new(std::sync::Arc::new(()), [3; 16], [4; 16], 1, "gpt-5.5").unwrap();
    assert_ne!(turn.identity.session, child.identity.session);
    let next_run = CodexTurn::new(std::sync::Arc::new(()), [1; 16], [3; 16], 1, "gpt-5.5").unwrap();
    assert_eq!(turn.identity.session, next_run.identity.session);
    assert_ne!(turn.request_id(), next_run.request_id());
    let first = turn.request_id();
    turn.sequence += 1;
    assert_ne!(turn.request_id(), first);
}
