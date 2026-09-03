use serde_json::{Value, json};

use super::{
    OpenCodeResponseRequest, ProviderContentPart, ProviderInputItem, ProviderMessageRole,
    ProviderTool,
};
use crate::provider::{OpenCodeService, ProviderError};

fn request() -> OpenCodeResponseRequest {
    OpenCodeResponseRequest::new(
        OpenCodeService::Zen,
        "gpt-5.6-luna",
        128,
        512,
        vec![ProviderInputItem::Message {
            role: ProviderMessageRole::User,
            text: "hello".to_owned(),
            phase: None,
        }],
        vec![ProviderTool {
            name: "read_file".to_owned(),
            description: "Read one file".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
                "additionalProperties": false
            }),
        }],
    )
    .expect("request should be valid")
}

#[test]
fn request_has_a_bounded_stable_responses_shape() {
    let request = request();
    let body: Value = serde_json::from_slice(
        &request
            .encode_body()
            .expect("request should serialize after validation"),
    )
    .expect("request body should be JSON");
    assert_eq!(body["model"], "gpt-5.6-luna");
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert_eq!(body["parallel_tool_calls"], false);
    assert_eq!(body["include"][0], "reasoning.encrypted_content");
    assert_eq!(body["input"][0]["role"], "user");
    assert_eq!(body["input"][0]["content"], "hello");
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["strict"], true);
    assert!(!format!("{request:?}").contains("hello"));
}

#[test]
fn multimodal_request_uses_bounded_data_urls_only_for_reviewed_vision_models() {
    let image = morons_image::normalize_rgba(1, 1, vec![10, 20, 30, 255])
        .expect("fixture should normalize");
    let message = ProviderInputItem::MultimodalMessage {
        role: ProviderMessageRole::User,
        parts: vec![
            ProviderContentPart::Text("[picture.png] describe this".to_owned()),
            ProviderContentPart::Image {
                media_type: image.media_type,
                width: image.width,
                height: image.height,
                bytes: image.bytes.clone(),
            },
        ],
        phase: None,
    };
    let request = OpenCodeResponseRequest::new(
        OpenCodeService::Zen,
        "gpt-5.4",
        8_192,
        512,
        vec![message.clone()],
        Vec::new(),
    )
    .expect("vision request should validate");
    let body: Value = serde_json::from_slice(&request.encode_body().expect("body should encode"))
        .expect("body should be JSON");
    assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
    assert_eq!(body["input"][0]["content"][1]["type"], "input_image");
    assert!(
        body["input"][0]["content"][1]["image_url"]
            .as_str()
            .is_some_and(|url| url.starts_with("data:image/png;base64,"))
    );
    assert!(!format!("{message:?}").contains(&morons_image::encode_base64(&image.bytes)));
    assert_eq!(
        OpenCodeResponseRequest::new(
            OpenCodeService::Zen,
            "muse-spark-1.2",
            8_192,
            512,
            vec![message],
            Vec::new(),
        )
        .expect_err("non-vision model should reject image input"),
        ProviderError::InvalidRequest
    );
}

#[test]
fn request_preserves_bounded_ephemeral_reasoning_continuation() {
    let request = OpenCodeResponseRequest::new(
        OpenCodeService::Zen,
        "gpt-5.6-sol",
        16,
        16,
        vec![ProviderInputItem::Reasoning {
            id: "rs_1".to_owned(),
            summaries: vec!["summary".to_owned()],
            encrypted_content: Some("opaque-continuation".to_owned()),
        }],
        Vec::new(),
    )
    .expect("reasoning continuation should be valid");
    let body: Value = serde_json::from_slice(
        &request
            .encode_body()
            .expect("request should serialize after validation"),
    )
    .expect("request body should be JSON");
    assert_eq!(body["input"][0]["type"], "reasoning");
    assert_eq!(body["input"][0]["summary"][0]["text"], "summary");
    assert_eq!(body["input"][0]["encrypted_content"], "opaque-continuation");
    assert!(!format!("{request:?}").contains("opaque-continuation"));

    let unsupported_continuation = OpenCodeResponseRequest::new(
        OpenCodeService::Zen,
        "grok-4.6",
        16,
        16,
        vec![ProviderInputItem::Reasoning {
            id: "rs_1".to_owned(),
            summaries: Vec::new(),
            encrypted_content: Some("opaque-continuation".to_owned()),
        }],
        Vec::new(),
    );
    assert_eq!(
        unsupported_continuation.expect_err("unsupported continuation must fail"),
        ProviderError::InvalidRequest
    );
}

#[test]
fn request_rejects_unreviewed_models_and_malformed_tool_input() {
    let unsupported = OpenCodeResponseRequest::new(
        OpenCodeService::Go,
        "gpt-5.6-sol",
        1,
        1,
        vec![ProviderInputItem::Message {
            role: ProviderMessageRole::User,
            text: "hello".to_owned(),
            phase: None,
        }],
        Vec::new(),
    );
    assert_eq!(
        unsupported.expect_err("model must be rejected"),
        ProviderError::UnsupportedModel
    );

    let malformed_arguments = OpenCodeResponseRequest::new(
        OpenCodeService::Zen,
        "gpt-5.6-sol",
        1,
        1,
        vec![ProviderInputItem::FunctionCall {
            call_id: "call_1".to_owned(),
            name: "read_file".to_owned(),
            arguments: "[]".to_owned(),
        }],
        Vec::new(),
    );
    assert_eq!(
        malformed_arguments.expect_err("arguments must be rejected"),
        ProviderError::InvalidRequest
    );
}
