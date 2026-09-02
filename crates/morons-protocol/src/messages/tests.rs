use serde_json::{Value, json};

use super::{ClientMessage, ServerMessage};
use crate::{
    ApplicationError, ApplicationEvent, ApplicationRequest, ApplicationResponse, MutationRequestId,
    PROTOCOL_VERSION, SessionCatalogEventCursor, SessionId, SessionSummary,
};

const TEST_CLIENT_VERSION: &str = "test-client-version";

#[test]
fn client_hello_has_stable_json_shape() {
    let message = ClientMessage::hello(TEST_CLIENT_VERSION);
    let encoded = message.encode_json().expect("client hello should encode");
    let actual: Value = serde_json::from_slice(&encoded).expect("encoded message should be JSON");

    assert_eq!(
        actual,
        json!({
            "type": "hello",
            "protocol_version": PROTOCOL_VERSION,
            "client_version": TEST_CLIENT_VERSION,
        })
    );
}

#[test]
fn application_messages_have_stable_json_shapes() {
    let request = ClientMessage::request(
        7,
        ApplicationRequest::CreateSession {
            mutation_request_id: MutationRequestId::from_bytes([0x11; 16]),
            display_name: None,
            working_directory: "/projects/example".to_owned(),
        },
    );
    let response = ServerMessage::response(
        7,
        ApplicationResponse::SessionCreated {
            session: SessionSummary {
                id: SessionId::from_bytes([0x22; 16]),
                display_name: None,
                working_directory: Some("/projects/example".to_owned()),
                created_at_milliseconds: 42,
            },
        },
    );
    let failure = ServerMessage::request_failed(7, ApplicationError::RequestConflict);
    let event = ServerMessage::event(ApplicationEvent::SessionCreated {
        cursor: SessionCatalogEventCursor::from_bytes(8_u64.to_be_bytes()),
        session: SessionSummary {
            id: SessionId::from_bytes([0x23; 16]),
            display_name: Some("Event session".to_owned()),
            working_directory: Some("/projects/example".to_owned()),
            created_at_milliseconds: 43,
        },
    });

    assert_eq!(
        serde_json::to_value(request).expect("request should encode"),
        json!({
            "type": "request",
            "request_id": 7,
            "request": {
                "operation": "create_session",
                "mutation_request_id": "mut_11111111111111111111111111111111",
                "display_name": null,
                "working_directory": "/projects/example",
            },
        })
    );
    assert_eq!(
        serde_json::to_value(response).expect("response should encode"),
        json!({
            "type": "response",
            "request_id": 7,
            "response": {
                "result": "session_created",
                "session": {
                    "id": "ses_22222222222222222222222222222222",
                    "display_name": null,
                    "working_directory": "/projects/example",
                    "created_at_milliseconds": 42,
                },
            },
        })
    );
    assert_eq!(
        serde_json::to_value(failure).expect("failure should encode"),
        json!({
            "type": "request_failed",
            "request_id": 7,
            "error": { "code": "request_conflict" },
        })
    );
    assert_eq!(
        serde_json::to_value(event).expect("event should encode"),
        json!({
            "type": "event",
            "event": {
                "event": "session_created",
                "cursor": "scc1_0000000000000008",
                "session": {
                    "id": "ses_23232323232323232323232323232323",
                    "display_name": "Event session",
                    "working_directory": "/projects/example",
                    "created_at_milliseconds": 43,
                },
            },
        })
    );
}

#[test]
fn messages_round_trip_through_json() {
    let client = ClientMessage::request(
        9,
        ApplicationRequest::GetSession {
            session_id: SessionId::from_bytes([0x33; 16]),
        },
    );
    let server = ServerMessage::request_failed(9, ApplicationError::SessionNotFound);

    assert_eq!(
        ClientMessage::decode_json(&client.encode_json().expect("message should encode"))
            .expect("message should decode"),
        client
    );
    assert_eq!(
        ServerMessage::decode_json(&server.encode_json().expect("message should encode"))
            .expect("message should decode"),
        server
    );
}

#[test]
fn client_message_rejects_unknown_fields() {
    let encoded = serde_json::to_vec(&json!({
        "type": "hello",
        "protocol_version": PROTOCOL_VERSION,
        "client_version": TEST_CLIENT_VERSION,
        "extra": true,
    }))
    .expect("test message should encode");

    assert!(ClientMessage::decode_json(&encoded).is_err());
}

#[test]
fn protocol_version_mismatch_has_stable_json_shape() {
    let received_protocol_version = PROTOCOL_VERSION + 1;
    let message = ServerMessage::protocol_version_mismatch(received_protocol_version);
    let encoded = message.encode_json().expect("message should encode");
    let actual: Value = serde_json::from_slice(&encoded).expect("encoded message should be JSON");

    assert_eq!(
        actual,
        json!({
            "type": "protocol_version_mismatch",
            "expected_protocol_version": PROTOCOL_VERSION,
            "received_protocol_version": received_protocol_version,
        })
    );
}
