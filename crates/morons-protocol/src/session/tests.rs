use serde::Serialize;
use serde_json::{Value, json};

use super::{
    ApplicationError, ApplicationEvent, ApplicationRequest, ApplicationResponse, MutationRequestId,
    ResourceLimit, SessionCatalogEventCursor, SessionEventCursor, SessionId, SessionListCursor,
    SessionSummary, SkillSource, SkillSummary, WorkspaceState, WorkspaceSummary,
};
use crate::{
    ClientMessage, LocalCommandId, LocalCommandStatus, MessageId, OpenCodeApiKey,
    OpenCodeCredentialStatus, OpenCodeModelCapabilities, OpenCodeModelRetention,
    OpenCodeModelSummary, OpenCodeModelTrainingUse, OpenCodeService, RunFailureKind, RunId,
    RunState, RunSummary, ToolCallId, ToolKind, ToolResultStatus,
};

const TEST_API_KEY: &str = "not-a-real-protocol-key";

#[test]
fn application_request_has_stable_json_shape() {
    let request = ApplicationRequest::CreateSession {
        mutation_request_id: MutationRequestId::from_bytes([0x11; 16]),
        display_name: Some("A session".to_owned()),
        working_directory: "/projects/example".to_owned(),
    };
    let debug = format!("{request:?}");
    assert!(!debug.contains("/projects/example"));
    assert!(debug.contains("working_directory_bytes"));
    let actual = serde_json::to_value(request).expect("request should encode");

    assert_eq!(
        actual,
        json!({
            "operation": "create_session",
            "mutation_request_id": "mut_11111111111111111111111111111111",
            "display_name": "A session",
            "working_directory": "/projects/example",
        })
    );
}

#[test]
fn session_skill_catalog_has_stable_json_shapes() {
    let session_id = SessionId::from_bytes([0x18; 16]);
    let request = ApplicationRequest::ListSessionSkills { session_id };
    assert_eq!(
        serde_json::to_value(request).expect("skill request should encode"),
        json!({
            "operation": "list_session_skills",
            "session_id": "ses_18181818181818181818181818181818",
        })
    );
    let response = ApplicationResponse::SessionSkillsListed {
        session_id,
        skills: vec![SkillSummary {
            name: "skill-creator".to_owned(),
            description: "Creates Agent Skills.".to_owned(),
            source: SkillSource::Bundled,
        }],
        warnings: Vec::new(),
    };
    assert_eq!(
        serde_json::to_value(response).expect("skill response should encode"),
        json!({
            "result": "session_skills_listed",
            "session_id": "ses_18181818181818181818181818181818",
            "skills": [{
                "name": "skill-creator",
                "description": "Creates Agent Skills.",
                "source": "bundled",
            }],
            "warnings": [],
        })
    );
}

#[test]
fn run_request_has_stable_json_shape() {
    let request = ApplicationRequest::SubmitSessionInput {
        mutation_request_id: MutationRequestId::from_bytes([0x12; 16]),
        session_id: SessionId::from_bytes([0x13; 16]),
        text: "sensitive prompt text".to_owned(),
        service: OpenCodeService::Zen,
        model_id: "muse-spark-1.2".to_owned(),
    };
    let debug = format!("{request:?}");
    assert!(!debug.contains("sensitive prompt text"));
    assert!(debug.contains("text_bytes"));

    assert_eq!(
        serde_json::to_value(request).expect("run request should encode"),
        json!({
            "operation": "submit_session_input",
            "mutation_request_id": "mut_12121212121212121212121212121212",
            "session_id": "ses_13131313131313131313131313131313",
            "text": "sensitive prompt text",
            "service": "zen",
            "model_id": "muse-spark-1.2",
        })
    );
}

#[test]
fn local_command_contract_is_stable_and_debug_redacts_content() {
    let session_id = SessionId::from_bytes([0x21; 16]);
    let request = ApplicationRequest::ExecuteLocalCommand {
        mutation_request_id: MutationRequestId::from_bytes([0x22; 16]),
        session_id,
        command: "printf sensitive".to_owned(),
        context_visible: false,
    };
    assert!(!format!("{request:?}").contains("printf sensitive"));
    assert_eq!(
        serde_json::to_value(request).unwrap(),
        json!({
            "operation": "execute_local_command",
            "mutation_request_id": "mut_22222222222222222222222222222222",
            "session_id": "ses_21212121212121212121212121212121",
            "command": "printf sensitive",
            "context_visible": false,
        })
    );
    let entry = crate::TranscriptEntry::LocalCommand {
        id: MessageId::from_bytes([0x23; 16]),
        command_id: LocalCommandId::from_bytes([0x24; 16]),
        command: "printf sensitive".to_owned(),
        context_visible: false,
        status: LocalCommandStatus::Succeeded,
        exit_code: Some(0),
        signal: None,
        stdout: "sensitive output".to_owned(),
        stderr: String::new(),
        created_at_milliseconds: 9,
    };
    let debug = format!("{entry:?}");
    assert!(!debug.contains("sensitive"));
    assert_eq!(
        serde_json::to_value(entry).unwrap()["entry"],
        "local_command"
    );
}

#[test]
fn credential_request_has_stable_json_shape_and_redacted_debug() {
    let request = ApplicationRequest::SetOpenCodeCredential {
        mutation_request_id: MutationRequestId::from_bytes([0x12; 16]),
        expected_generation: 7,
        api_key: OpenCodeApiKey::new(TEST_API_KEY).expect("test API key should be valid"),
    };
    let debug = format!("{request:?}");
    assert!(!debug.contains(TEST_API_KEY));
    assert!(debug.contains("[REDACTED]"));
    let framed_debug = format!("{:?}", ClientMessage::request(1, request.clone()));
    assert!(!framed_debug.contains(TEST_API_KEY));
    assert!(framed_debug.contains("[REDACTED]"));

    assert_eq!(
        serde_json::to_value(request).expect("credential request should encode"),
        json!({
            "operation": "set_open_code_credential",
            "mutation_request_id": "mut_12121212121212121212121212121212",
            "expected_generation": 7,
            "api_key": TEST_API_KEY,
        })
    );
}

#[test]
fn structured_tool_and_uncertainty_contracts_have_stable_json_shapes() {
    let session_id = SessionId::from_bytes([0x71; 16]);
    let run_id = RunId::from_bytes([0x72; 16]);
    let call_id = ToolCallId::from_bytes([0x73; 16]);
    let entry = crate::TranscriptEntry::ToolResult {
        id: MessageId::from_bytes([0x74; 16]),
        run_id,
        call_id,
        tool: ToolKind::EditFile,
        status: ToolResultStatus::Uncertain,
        summary: "tool failed: workspace effect is uncertain".to_owned(),
        created_at_milliseconds: 42,
    };
    assert_eq!(
        serde_json::to_value(entry).expect("tool result should encode"),
        json!({
            "entry": "tool_result",
            "id": "msg_74747474747474747474747474747474",
            "run_id": "run_72727272727272727272727272727272",
            "call_id": "tool_73737373737373737373737373737373",
            "tool": "edit_file",
            "status": "uncertain",
            "summary": "tool failed: workspace effect is uncertain",
            "created_at_milliseconds": 42,
        })
    );

    let command = crate::TranscriptEntry::ToolCall {
        id: MessageId::from_bytes([0x76; 16]),
        run_id,
        call_id: ToolCallId::from_bytes([0x77; 16]),
        tool: ToolKind::RunCommand,
        path: ".".to_owned(),
        created_at_milliseconds: 43,
    };
    assert_eq!(
        serde_json::to_value(command).expect("command call should encode"),
        json!({
            "entry": "tool_call",
            "id": "msg_76767676767676767676767676767676",
            "run_id": "run_72727272727272727272727272727272",
            "call_id": "tool_77777777777777777777777777777777",
            "tool": "run_command",
            "path": ".",
            "created_at_milliseconds": 43,
        })
    );

    let request = ApplicationRequest::AcknowledgeToolUncertainty {
        mutation_request_id: MutationRequestId::from_bytes([0x75; 16]),
        session_id,
        run_id,
    };
    assert_eq!(
        serde_json::to_value(request).expect("acknowledgement should encode"),
        json!({
            "operation": "acknowledge_tool_uncertainty",
            "mutation_request_id": "mut_75757575757575757575757575757575",
            "session_id": "ses_71717171717171717171717171717171",
            "run_id": "run_72727272727272727272727272727272",
        })
    );
}

#[test]
fn server_stop_contract_has_stable_json_shape() {
    let request = ApplicationRequest::StopServer {
        mutation_request_id: MutationRequestId::from_bytes([0x14; 16]),
    };
    assert_eq!(
        serde_json::to_value(request).expect("server stop should encode"),
        json!({
            "operation": "stop_server",
            "mutation_request_id": "mut_14141414141414141414141414141414",
        })
    );
    assert_eq!(
        serde_json::to_value(ApplicationResponse::ServerStopAccepted {
            current_server_stopping: true,
        })
        .expect("server stop result should encode"),
        json!({
            "result": "server_stop_accepted",
            "current_server_stopping": true,
        })
    );
}

#[test]
fn model_catalog_contract_has_stable_json_shape() {
    let request = ApplicationRequest::ListOpenCodeModels {
        service: OpenCodeService::Go,
    };
    assert_eq!(
        serde_json::to_value(request).expect("model query should encode"),
        json!({
            "operation": "list_open_code_models",
            "service": "go",
        })
    );

    let response = ApplicationResponse::OpenCodeModelsListed {
        service: OpenCodeService::Go,
        models: vec![OpenCodeModelSummary {
            service: OpenCodeService::Go,
            id: "grok-4.6".to_owned(),
            display_name: "Grok 4.6".to_owned(),
            available: true,
            responses_protocol_revision: 1,
            capabilities: OpenCodeModelCapabilities {
                text_input: true,
                text_output: true,
                reasoning: true,
                reasoning_continuation: false,
                tool_calls: true,
            },
            maximum_input_tokens: 96_000,
            maximum_output_tokens: 32_000,
            training_use: OpenCodeModelTrainingUse::NotUsed,
            retention: OpenCodeModelRetention::UpToThirtyDays,
        }],
    };
    assert_eq!(
        serde_json::to_value(response).expect("model response should encode"),
        json!({
            "result": "open_code_models_listed",
            "service": "go",
            "models": [{
                "service": "go",
                "id": "grok-4.6",
                "display_name": "Grok 4.6",
                "available": true,
                "responses_protocol_revision": 1,
                "capabilities": {
                    "text_input": true,
                    "text_output": true,
                    "reasoning": true,
                    "reasoning_continuation": false,
                    "tool_calls": true,
                },
                "maximum_input_tokens": 96000,
                "maximum_output_tokens": 32000,
                "training_use": "not_used",
                "retention": "up_to_thirty_days",
            }],
        })
    );
}

#[test]
fn credential_status_has_stable_json_shape() {
    let response = ApplicationResponse::OpenCodeCredentialStatus {
        credential: OpenCodeCredentialStatus {
            configured: true,
            generation: 7,
        },
    };
    assert_eq!(
        serde_json::to_value(response).expect("credential status should encode"),
        json!({
            "result": "open_code_credential_status",
            "credential": {
                "configured": true,
                "generation": 7,
            },
        })
    );
}

#[test]
fn application_response_has_stable_json_shape() {
    let mut list_cursor = [0_u8; 16];
    list_cursor[..8].copy_from_slice(&9_u64.to_be_bytes());
    list_cursor[8..].copy_from_slice(&7_u64.to_be_bytes());
    let response = ApplicationResponse::SessionsListed {
        sessions: vec![SessionSummary {
            id: SessionId::from_bytes([0x22; 16]),
            display_name: None,
            working_directory: Some("/projects/example".to_owned()),
            created_at_milliseconds: 42,
        }],
        next_cursor: Some(SessionListCursor::from_bytes(list_cursor)),
        catalog_cursor: SessionCatalogEventCursor::from_bytes(9_u64.to_be_bytes()),
    };
    let actual = serde_json::to_value(response).expect("response should encode");

    assert_eq!(
        actual,
        json!({
            "result": "sessions_listed",
            "sessions": [{
                "id": "ses_22222222222222222222222222222222",
                "display_name": null,
                "working_directory": "/projects/example",
                "created_at_milliseconds": 42,
            }],
            "next_cursor": "sc2_00000000000000090000000000000007",
            "catalog_cursor": "scc1_0000000000000009",
        })
    );
}

#[test]
fn transcript_snapshot_response_has_stable_json_shape() {
    let session_id = SessionId::from_bytes([0x23; 16]);
    let response = ApplicationResponse::SessionTranscriptListed {
        session: SessionSummary {
            id: session_id,
            display_name: None,
            working_directory: None,
            created_at_milliseconds: 42,
        },
        workspace: WorkspaceSummary {
            state: WorkspaceState::Ready,
            file_count: 7,
            logical_bytes: 42,
            block_reason: None,
            blocked_run_id: None,
            blocked_tool: None,
        },
        entries: Vec::new(),
        runs: Vec::new(),
        active_run_id: None,
        active_command_id: None,
        next_cursor: None,
        event_cursor: session_event_cursor(session_id, 9),
    };
    assert_eq!(
        serde_json::to_value(response).expect("transcript snapshot should encode"),
        json!({
            "result": "session_transcript_listed",
            "session": {
                "id": "ses_23232323232323232323232323232323",
                "display_name": null,
                "working_directory": null,
                "created_at_milliseconds": 42,
            },
            "workspace": {
                "state": "ready",
                "file_count": 7,
                "logical_bytes": 42,
                "block_reason": null,
                "blocked_run_id": null,
                "blocked_tool": null,
            },
            "entries": [],
            "runs": [],
            "active_run_id": null,
            "active_command_id": null,
            "next_cursor": null,
            "event_cursor": "sec1_232323232323232323232323232323230000000000000009",
        })
    );
}

#[test]
fn workspace_event_has_stable_json_shape() {
    let session_id = SessionId::from_bytes([0x35; 16]);
    let event = ApplicationEvent::SessionWorkspaceChanged {
        cursor: session_event_cursor(session_id, 7),
        session_id,
        workspace: WorkspaceSummary {
            state: WorkspaceState::Importing,
            file_count: 0,
            logical_bytes: 0,
            block_reason: None,
            blocked_run_id: None,
            blocked_tool: None,
        },
    };
    assert_eq!(
        serde_json::to_value(event).expect("workspace event should encode"),
        json!({
            "event": "session_workspace_changed",
            "cursor": "sec1_353535353535353535353535353535350000000000000007",
            "session_id": "ses_35353535353535353535353535353535",
            "workspace": {
                "state": "importing",
                "file_count": 0,
                "logical_bytes": 0,
                "block_reason": null,
                "blocked_run_id": null,
                "blocked_tool": null,
            },
        })
    );
}

#[test]
fn run_response_has_stable_json_shape() {
    let response = ApplicationResponse::RunFound {
        run: RunSummary {
            id: RunId::from_bytes([0x31; 16]),
            session_id: SessionId::from_bytes([0x32; 16]),
            user_message_id: MessageId::from_bytes([0x33; 16]),
            service: OpenCodeService::Go,
            model_id: "grok-4.6".to_owned(),
            protocol_revision: 1,
            credential_generation: 4,
            context_policy_version: 1,
            tool_catalog_version: 1,
            tool_limits_version: 1,
            state: RunState::Failed,
            cancellation_requested: false,
            failure: Some(RunFailureKind::RateLimited),
            accepted_at_milliseconds: 41,
            updated_at_milliseconds: 42,
        },
    };

    assert_eq!(
        serde_json::to_value(response).expect("run response should encode"),
        json!({
            "result": "run_found",
            "run": {
                "id": "run_31313131313131313131313131313131",
                "session_id": "ses_32323232323232323232323232323232",
                "user_message_id": "msg_33333333333333333333333333333333",
                "service": "go",
                "model_id": "grok-4.6",
                "protocol_revision": 1,
                "credential_generation": 4,
                "context_policy_version": 1,
                "tool_catalog_version": 1,
                "tool_limits_version": 1,
                "state": "failed",
                "cancellation_requested": false,
                "failure": "rate_limited",
                "accepted_at_milliseconds": 41,
                "updated_at_milliseconds": 42,
            },
        })
    );
}

#[test]
fn application_event_has_stable_json_shape() {
    let event = ApplicationEvent::SessionCreated {
        cursor: SessionCatalogEventCursor::from_bytes(9_u64.to_be_bytes()),
        session: SessionSummary {
            id: SessionId::from_bytes([0x22; 16]),
            display_name: Some("Created".to_owned()),
            working_directory: Some("/projects/example".to_owned()),
            created_at_milliseconds: 42,
        },
    };

    assert_eq!(
        serde_json::to_value(event).expect("event should encode"),
        json!({
            "event": "session_created",
            "cursor": "scc1_0000000000000009",
            "session": {
                "id": "ses_22222222222222222222222222222222",
                "display_name": "Created",
                "working_directory": "/projects/example",
                "created_at_milliseconds": 42,
            },
        })
    );
}

#[test]
fn session_subscription_contract_has_stable_json_shapes_and_redacted_delta_debug() {
    let session_id = SessionId::from_bytes([0x51; 16]);
    let cursor = session_event_cursor(session_id, 9);
    let request = ApplicationRequest::SubscribeSession { session_id, cursor };
    assert_eq!(
        serde_json::to_value(request).expect("subscription request should encode"),
        json!({
            "operation": "subscribe_session",
            "session_id": "ses_51515151515151515151515151515151",
            "cursor": "sec1_515151515151515151515151515151510000000000000009",
        })
    );

    let event = ApplicationEvent::SessionAssistantDelta {
        session_id,
        run_id: RunId::from_bytes([0x52; 16]),
        sequence: 3,
        delta: "sensitive partial output".to_owned(),
        refusal: false,
    };
    let debug = format!("{event:?}");
    assert!(!debug.contains("sensitive partial output"));
    assert!(debug.contains("delta_bytes"));
    assert_eq!(
        serde_json::to_value(event).expect("delta event should encode"),
        json!({
            "event": "session_assistant_delta",
            "session_id": "ses_51515151515151515151515151515151",
            "run_id": "run_52525252525252525252525252525252",
            "sequence": 3,
            "delta": "sensitive partial output",
            "refusal": false,
        })
    );
}

#[test]
fn application_error_has_stable_json_shape() {
    let error = ApplicationError::ResourceLimit {
        resource: ResourceLimit::Sessions,
    };
    let actual = serde_json::to_value(error).expect("error should encode");

    assert_eq!(
        actual,
        json!({
            "code": "resource_limit",
            "resource": "sessions",
        })
    );
}

#[test]
fn opaque_values_round_trip_through_json() {
    let session_id = SessionId::from_bytes([0x33; 16]);
    let mutation_id = MutationRequestId::from_bytes([0x44; 16]);
    let list_cursor = SessionListCursor::from_bytes([0x55; 16]);
    let catalog_cursor = SessionCatalogEventCursor::from_bytes([0x66; 8]);
    let event_cursor = SessionEventCursor::from_bytes([0x77; 24]);

    assert_eq!(round_trip(&session_id), session_id);
    assert_eq!(round_trip(&mutation_id), mutation_id);
    assert_eq!(round_trip(&list_cursor), list_cursor);
    assert_eq!(round_trip(&catalog_cursor), catalog_cursor);
    assert_eq!(round_trip(&event_cursor), event_cursor);
}

#[test]
fn malformed_opaque_values_are_rejected() {
    for encoded in [
        "ses_1111111111111111111111111111111",
        "ses_1111111111111111111111111111111g",
        "ses_1111111111111111111111111111111A",
        "mut_11111111111111111111111111111111",
    ] {
        assert!(serde_json::from_value::<SessionId>(Value::String(encoded.to_owned())).is_err());
    }
    assert!(
        serde_json::from_value::<SessionListCursor>(Value::String(
            "sc1_0000000000000001".to_owned()
        ))
        .is_err()
    );
    assert!(
        serde_json::from_value::<SessionCatalogEventCursor>(Value::String(
            "sec1_0000000000000001".to_owned()
        ))
        .is_err()
    );
    assert!(
        serde_json::from_value::<SessionEventCursor>(Value::String(
            "sec1_0000000000000001".to_owned()
        ))
        .is_err()
    );
}

#[test]
fn application_request_rejects_unknown_fields() {
    let encoded = json!({
        "operation": "list_sessions",
        "cursor": null,
        "limit": 10,
        "extra": true,
    });

    assert!(serde_json::from_value::<ApplicationRequest>(encoded).is_err());
}

#[test]
fn removed_repository_import_operation_is_rejected() {
    let encoded = json!({
        "operation": "import_repository",
        "mutation_request_id": "mut_31313131313131313131313131313131",
        "session_id": "ses_32323232323232323232323232323232",
        "source_path": "/private/repository",
    });

    assert!(serde_json::from_value::<ApplicationRequest>(encoded).is_err());
}

fn session_event_cursor(session_id: SessionId, sequence: u64) -> SessionEventCursor {
    let mut bytes = [0_u8; 24];
    bytes[..16].copy_from_slice(session_id.as_bytes());
    bytes[16..].copy_from_slice(&sequence.to_be_bytes());
    SessionEventCursor::from_bytes(bytes)
}

fn round_trip<T>(value: &T) -> T
where
    T: Serialize + serde::de::DeserializeOwned,
{
    let encoded = serde_json::to_vec(value).expect("value should encode");
    serde_json::from_slice(&encoded).expect("value should decode")
}
