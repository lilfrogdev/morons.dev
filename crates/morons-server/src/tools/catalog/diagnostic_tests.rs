use super::*;
use DebugNormalizationStage as Stage;

fn call(name: &str, value: Value) -> ProviderToolCall {
    ProviderToolCall {
        provider_item_id: None,
        provider_call_id: "local_synthetic_call".to_owned(),
        name: name.to_owned(),
        arguments: serde_json::to_string(&value).unwrap(),
        opaque_continuation: None,
    }
}

fn reject(calls: Vec<ProviderToolCall>, catalog: u16, expected: Stage, resource: bool) {
    let mut stage = Stage::Other;
    let result = parse_provider_calls_diagnosed(calls, catalog, &mut stage);
    assert!(if resource {
        matches!(result, Err(ToolCallValidationError::ResourceLimit))
    } else {
        matches!(result, Err(ToolCallValidationError::InvalidProviderOutput))
    });
    assert_eq!(stage, expected);
    assert!(!serde_json::to_string(&stage).unwrap().contains("PRIVATE"));
}

#[test]
fn diagnostic_catalog_count_json_and_duplicate_order() {
    reject(Vec::new(), TOOL_CATALOG_VERSION, Stage::CallCount, false);
    reject(
        vec![call("read", json!({"path":"x"}))],
        0,
        Stage::Catalog,
        false,
    );
    reject(
        (0..=MAX_TOOL_CALLS_PER_TURN)
            .map(|_| call("read", json!({"path":"x"})))
            .collect(),
        TOOL_CATALOG_VERSION,
        Stage::CallCount,
        true,
    );
    let mut malformed = call("PRIVATE", json!(null));
    malformed.arguments = "{".to_owned();
    reject(
        vec![malformed],
        TOOL_CATALOG_VERSION,
        Stage::ArgumentJson,
        false,
    );
    let mut second = call("PRIVATE", json!(null));
    second.arguments = "{".to_owned();
    reject(
        vec![call("read", json!({"path":"x"})), second],
        TOOL_CATALOG_VERSION,
        Stage::DuplicateCallId,
        false,
    );
}

#[test]
fn diagnostic_read_stages_and_unchanged_valid_defaults() {
    for (value, stage) in [
        (json!({"PRIVATE":"x"}), Stage::ToolFieldSet),
        (json!({"path":"x","offset":"PRIVATE"}), Stage::ToolType),
        (json!({"path":"x","offset":0}), Stage::ReadWindow),
        (json!({"path":"\u{0}"}), Stage::Path),
    ] {
        reject(
            vec![call("read", value)],
            TOOL_CATALOG_VERSION,
            stage,
            false,
        );
    }
    let mut stage = Stage::Other;
    let result = parse_provider_calls_diagnosed(
        vec![call("read", json!({"path":"x"}))],
        TOOL_CATALOG_VERSION,
        &mut stage,
    )
    .unwrap();
    assert_eq!(result.len(), 1);
    assert!(matches!(
        &result[0].input,
        ToolInput::Read {
            offset: 1,
            limit: MAX_READ_LINES,
            ..
        }
    ));
    assert!(validate_canonical_input(&result[0].input));
    assert!(!crate::debug_log::enabled());
}

#[test]
fn diagnostic_edit_shape_and_byte_guards() {
    for value in [
        json!({"path":"x","replacements":[]}),
        json!({"path":"x","replacements":[{"old_text":"","new_text":"x"}]}),
    ] {
        reject(
            vec![call("edit", value)],
            TOOL_CATALOG_VERSION,
            Stage::EditBounds,
            false,
        );
    }
    reject(
        vec![call(
            "edit",
            json!({"path":"x","replacements":[{"old_text":"x","new_text":"y","PRIVATE":true}]}),
        )],
        TOOL_CATALOG_VERSION,
        Stage::ToolType,
        false,
    );
    reject(
        vec![call(
            "edit",
            json!({"path":"x","replacements":[{"old_text":"x","new_text":"x".repeat(MAX_REPLACEMENT_BYTES)}]}),
        )],
        TOOL_CATALOG_VERSION,
        Stage::OutputBytes,
        true,
    );
}

#[test]
fn diagnostic_task_unknown_and_forbidden_child_tools() {
    reject(
        vec![call("PRIVATE\u{1b}", json!({}))],
        TOOL_CATALOG_VERSION,
        Stage::UnknownTool,
        false,
    );
    reject(
        vec![call("task", json!({"context":"","tasks":[]}))],
        TOOL_CATALOG_VERSION,
        Stage::TaskBounds,
        false,
    );
    for forbidden in [
        call("ipython", json!({"cell":"print(1)"})),
        call(
            "task",
            json!({"context":"scope","tasks":[{"task":"inspect"}]}),
        ),
    ] {
        let mut stage = Stage::Other;
        assert!(matches!(
            parse_subagent_provider_calls_diagnosed(vec![forbidden], &mut stage),
            Err(ToolCallValidationError::InvalidProviderOutput)
        ));
        assert_eq!(stage, Stage::ForbiddenChildTool);
    }
}

#[test]
fn diagnostic_semantic_and_resource_guards_remain_distinct() {
    reject(
        vec![call("bash", json!({"command":""}))],
        TOOL_CATALOG_VERSION,
        Stage::Other,
        false,
    );
    reject(
        vec![call(
            "bash",
            json!({"command":"x".repeat(MAX_BASH_COMMAND_BYTES+1)}),
        )],
        TOOL_CATALOG_VERSION,
        Stage::OutputBytes,
        true,
    );
    reject(
        vec![call("web_search", json!({"query":"PRIVATE\n"}))],
        TOOL_CATALOG_VERSION,
        Stage::Other,
        false,
    );
    reject(
        vec![call(
            "web_search",
            json!({"query":"x".repeat(MAX_WEB_SEARCH_QUERY_BYTES+1)}),
        )],
        TOOL_CATALOG_VERSION,
        Stage::OutputBytes,
        true,
    );
    assert_eq!(crate::debug_log::next_attempt_id(), None);
}
