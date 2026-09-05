use super::*;

fn input() -> Vec<ProviderInputItem> {
    vec![ProviderInputItem::Message {
        role: ProviderMessageRole::User,
        text: "inspect the project".to_owned(),
        phase: None,
    }]
}

#[test]
fn prepared_and_fresh_tools_encode_identically_for_every_wire_family() {
    for tools in [
        crate::tools::provider_tools().unwrap(),
        crate::tools::subagent_provider_tools().unwrap(),
    ] {
        for (service, model) in [
            (OpenCodeService::Zen, "gpt-5.6-luna"),
            (OpenCodeService::Go, "glm-5.3-flash"),
            (OpenCodeService::Go, "qwen3.8-max"),
            (OpenCodeService::Zen, "gemini-3.6-flash"),
        ] {
            let prepared = OpenCodeResponseRequest::with_prepared_tools(
                [1; 16],
                service,
                model,
                10_000,
                128,
                input(),
                tools,
            )
            .unwrap();
            let fresh = OpenCodeResponseRequest::new(
                [1; 16],
                service,
                model,
                10_000,
                128,
                input(),
                tools.definitions().to_vec(),
            )
            .unwrap();
            assert_eq!(prepared.encoded_body(), fresh.encoded_body());
        }
        assert_eq!(
            tools.gemini_parameters().unwrap().as_ptr(),
            tools.gemini_parameters().unwrap().as_ptr()
        );
    }
    assert!(std::ptr::eq(
        crate::tools::provider_tools().unwrap(),
        crate::tools::provider_tools().unwrap()
    ));
}

#[test]
fn prepared_tools_validate_dynamic_definitions_and_isolate_projection_errors() {
    let invalid = ProviderTool {
        name: "invalid name".to_owned(),
        description: "test".to_owned(),
        parameters: serde_json::json!({}),
    };
    assert!(PreparedProviderTools::new(vec![invalid]).is_err());
    let tool = ProviderTool {
        name: "custom".to_owned(),
        description: "test".to_owned(),
        parameters: serde_json::json!({"type":123}),
    };
    let tools = PreparedProviderTools::new(vec![tool]).unwrap();
    assert!(tools.gemini_parameters().is_err());
    assert!(tools.gemini_parameters().is_err());
    let private = PreparedProviderTools::new(vec![ProviderTool {
        name: "custom".to_owned(),
        description: "private-description".to_owned(),
        parameters: serde_json::json!({"type":"string","const":"private-schema-text"}),
    }])
    .unwrap();
    private.gemini_parameters().unwrap();
    assert!(!format!("{private:?}").contains("private-"));
    assert!(
        OpenCodeResponseRequest::with_prepared_tools(
            [1; 16],
            OpenCodeService::Zen,
            "gpt-5.6-luna",
            100,
            128,
            input(),
            &tools
        )
        .is_ok()
    );
}

#[test]
#[ignore = "manual local timing probe; no network, real credentials or timing assertions"]
fn measure_prepared_tool_requests() {
    use std::{hint::black_box, time::Instant};
    let tools = crate::tools::provider_tools().unwrap();
    for (service, model) in [
        (OpenCodeService::Zen, "gpt-5.6-luna"),
        (OpenCodeService::Zen, "gemini-3.6-flash"),
    ] {
        let start = Instant::now();
        for _ in 0..2_000 {
            black_box(
                OpenCodeResponseRequest::new(
                    [1; 16],
                    service,
                    model,
                    10_000,
                    128,
                    input(),
                    tools.definitions().to_vec(),
                )
                .unwrap(),
            );
        }
        let fresh = start.elapsed();
        let start = Instant::now();
        for _ in 0..2_000 {
            black_box(
                OpenCodeResponseRequest::with_prepared_tools(
                    [1; 16],
                    service,
                    model,
                    10_000,
                    128,
                    input(),
                    tools,
                )
                .unwrap(),
            );
        }
        eprintln!(
            "{model}: 2000 fresh={fresh:?}, prepared={:?}",
            start.elapsed()
        );
    }
}
