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
fn builtin_responses_tools_use_closed_strict_schemas_with_nullable_task_names() {
    fn assert_closed(schema: &Value) {
        if schema["type"] == "object" {
            let properties = schema["properties"].as_object().unwrap();
            let required = schema["required"].as_array().unwrap();
            assert_eq!(schema["additionalProperties"], false);
            assert_eq!(properties.len(), required.len());
            for (name, property) in properties {
                assert!(required.iter().any(|field| field == name));
                assert_closed(property);
            }
        }
        if let Some(items) = schema.get("items") {
            assert_closed(items);
        }
    }
    for tools in [
        crate::tools::provider_tools().unwrap(),
        crate::tools::subagent_provider_tools().unwrap(),
    ] {
        let request = OpenCodeResponseRequest::with_prepared_tools(
            [1; 16],
            OpenCodeService::Zen,
            "gpt-5.4-mini",
            10_000,
            128,
            input(),
            tools,
        )
        .unwrap();
        let body: Value = serde_json::from_slice(&request.encoded_body()).unwrap();
        for tool in body["tools"].as_array().unwrap() {
            assert_eq!(tool["strict"], true);
            assert_closed(&tool["parameters"]);
            if tool["name"] == "task" {
                assert_eq!(
                    tool["parameters"]["properties"]["tasks"]["items"]["properties"]["name"]["type"],
                    serde_json::json!(["string", "null"])
                );
            }
        }
    }
}

#[test]
fn gemini_read_descriptions_retain_limits_that_numeric_schema_lowering_omits() {
    for tools in [
        crate::tools::provider_tools().unwrap(),
        crate::tools::subagent_provider_tools().unwrap(),
    ] {
        let request = OpenCodeResponseRequest::with_prepared_tools(
            [1; 16],
            OpenCodeService::Zen,
            "gemini-3-flash",
            10_000,
            128,
            input(),
            tools,
        )
        .unwrap();
        let body: Value = serde_json::from_slice(&request.encoded_body()).unwrap();
        let read = body["tools"][0]["functionDeclarations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "read")
            .unwrap();
        let bounds = format!("1 through {}", crate::tools::MAX_READ_LINES);
        assert!(read["description"].as_str().unwrap().contains(&bounds));
        assert!(
            read["parameters"]["properties"]["limit"]["description"]
                .as_str()
                .unwrap()
                .contains(&bounds)
        );
        assert!(
            read["parameters"]["properties"]["limit"]
                .get("maximum")
                .is_none()
        );
    }
}

#[test]
fn prepared_tools_validate_dynamic_definitions_and_isolate_projection_errors() {
    let invalid = ProviderTool {
        strict: false,
        name: "invalid name".to_owned(),
        description: "test".to_owned(),
        parameters: serde_json::json!({}),
    };
    assert!(PreparedProviderTools::new(vec![invalid]).is_err());
    let tool = ProviderTool {
        name: "custom".to_owned(),
        description: "test".to_owned(),
        strict: false,
        parameters: serde_json::json!({"type":123}),
    };
    let tools = PreparedProviderTools::new(vec![tool]).unwrap();
    assert!(tools.gemini_parameters().is_err());
    assert!(tools.gemini_parameters().is_err());
    let private = PreparedProviderTools::new(vec![ProviderTool {
        name: "custom".to_owned(),
        description: "private-description".to_owned(),
        strict: false,
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
