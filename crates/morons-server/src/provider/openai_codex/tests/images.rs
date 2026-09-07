use super::*;
use crate::provider::ProviderContentPart;
#[test]
fn native_images_are_normalized_bounded_and_not_embedded_in_debug() {
    let turn = turn();
    let image = morons_image::normalize_rgba(1, 1, vec![10, 20, 30, 255]).unwrap();
    let part = ProviderContentPart::Image {
        media_type: image.media_type,
        width: image.width,
        height: image.height,
        bytes: image.bytes.clone(),
    };
    let input = vec![ProviderInputItem::MultimodalMessage {
        role: ProviderMessageRole::User,
        parts: vec![
            ProviderContentPart::Text("image prompt".into()),
            part.clone(),
        ],
        phase: None,
    }];
    let request = CodexRequest::new(
        &turn,
        "core",
        input,
        Vec::new(),
        CodexRequestLimits {
            estimated_input_tokens: 100,
            maximum_output_tokens: 32,
        },
        DataUseRestrictions::default(),
    )
    .unwrap();
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["input"][0]["content"][1]["type"], "input_image");
    assert!(
        body["input"][0]["content"][1]["image_url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,")
    );
    assert!(!format!("{request:?}").contains(&morons_image::encode_base64(&image.bytes)));
    for parts in [
        vec![part; 17],
        vec![ProviderContentPart::Image {
            media_type: image.media_type,
            width: 1,
            height: 1,
            bytes: vec![0; 12],
        }],
    ] {
        assert!(
            CodexRequest::new(
                &turn,
                "core",
                vec![ProviderInputItem::MultimodalMessage {
                    role: ProviderMessageRole::User,
                    parts,
                    phase: None
                }],
                Vec::new(),
                request.limits,
                DataUseRestrictions::default()
            )
            .is_err()
        );
    }
}
