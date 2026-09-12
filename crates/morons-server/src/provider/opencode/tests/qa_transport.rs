// Transport regressions independently qualified before adoption (ADR0050).
use super::super::TransportDiagnostic;
use super::*;
use crate::{
    debug_log::{DebugEvent, DebugFinish, DebugService, DebugStage},
    provider::ProviderOutcome,
};
use serde_json::json;

fn planned(service: OpenCodeService, model: &str) -> OpenCodeResponseRequest {
    OpenCodeResponseRequest::new(
        [0x41; 16],
        service,
        model,
        32,
        8192,
        vec![ProviderInputItem::Message {
            role: ProviderMessageRole::User,
            text: "hello".into(),
            phase: None,
        }],
        Vec::new(),
    )
    .unwrap()
}

fn wire(reason: &str) -> Value {
    json!({"id":"synthetic_response", "object":"chat.completion.chunk", "created":1,
        "model":"glm-5.3-flash", "choices":[{"index":0,"delta":{"role":"assistant","content":"ok"},"finish_reason":reason}],
        "usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}})
}

fn response_for(value: &Value, done: bool) -> Vec<u8> {
    response(
        "200 OK",
        "text/event-stream",
        format!(
            "data: {value}\n\n{}",
            if done { "data: [DONE]\n\n" } else { "" }
        )
        .as_bytes(),
    )
}

async fn observe(
    request: OpenCodeResponseRequest,
    bytes: Vec<u8>,
) -> (Result<ProviderOutcome, ProviderError>, DebugEvent) {
    let (base, capture, task) = spawn_single_response(bytes).await;
    let client = OpenCodeClient::for_test(endpoints(&base));
    let (_handle, mut cancellation) = provider_cancellation();
    let mut diagnostic = TransportDiagnostic::new(&request);
    let result = time::timeout(Duration::from_secs(5), async {
        let (response, deadline) = client
            .send_inference(
                TEST_KEY.as_bytes(),
                &request,
                request.encoded_body(),
                &mut cancellation,
                &mut diagnostic,
            )
            .await?;
        client
            .consume_inference(
                response,
                deadline,
                &request,
                &mut cancellation,
                |_| {},
                &mut diagnostic,
            )
            .await
    })
    .await
    .unwrap();
    let captured = time::timeout(Duration::from_secs(2), capture)
        .await
        .unwrap()
        .unwrap();
    task.await.unwrap();
    assert_eq!(captured.method, "POST");
    assert_eq!(
        captured.headers.get("authorization").unwrap(),
        &format!("Bearer {TEST_KEY}")
    );
    let body: Value = serde_json::from_slice(&captured.body).unwrap();
    assert_eq!(body["max_tokens"], 8192);
    assert_eq!(body["model"], request.model().id);
    assert_eq!(
        captured.path,
        if request.model().service == OpenCodeService::Go {
            "/zen/go/v1/chat/completions"
        } else {
            "/zen/v1/chat/completions"
        }
    );
    assert!(diagnostic.event(None, &result).is_none());
    let event = diagnostic.event(Some(41), &result).unwrap();
    let encoded = serde_json::to_string(&event).unwrap();
    for marker in [
        TEST_KEY,
        "synthetic_response",
        "SYNTHETIC_HOSTILE",
        "authorization",
        "synthetic_call",
    ] {
        assert!(!encoded.contains(marker));
    }
    (result, event)
}

fn check(
    event: DebugEvent,
    stage: DebugStage,
    finish: DebugFinish,
    done: bool,
    usage_seen: bool,
    error: Option<ProviderError>,
) {
    assert_eq!(
        event,
        DebugEvent::Provider {
            attempt_id: 41,
            service: DebugService::Go,
            protocol: 2,
            requested_output_tokens: 8192,
            stage,
            finish,
            done,
            usage_seen,
            receipt_accepted: error.is_none(),
            error
        }
    );
}

#[tokio::test]
async fn qa_non_success_status_after_successful_body_drain() {
    for (status, error) in [
        ("302 Found", ProviderError::RedirectDenied),
        (
            "401 Unauthorized",
            ProviderError::AuthenticationOrEntitlement,
        ),
        ("429 Too Many Requests", ProviderError::RateLimited),
        ("503 Service Unavailable", ProviderError::Unavailable),
        ("400 Bad Request", ProviderError::RequestRejected),
    ] {
        let (result, event) = observe(
            planned(OpenCodeService::Go, "glm-5.3-flash"),
            response(status, "text/plain", b"SYNTHETIC_HOSTILE"),
        )
        .await;
        assert_eq!(result.unwrap_err(), error);
        check(
            event,
            DebugStage::HttpStatus,
            DebugFinish::Absent,
            false,
            false,
            Some(error),
        );
    }
}

#[tokio::test]
async fn qa_non_success_oversized_body_precedes_status_rejection() {
    let mut body = vec![b'x'; super::super::MAX_ERROR_BODY_BYTES + 1];
    body[..17].copy_from_slice(b"SYNTHETIC_HOSTILE");
    let (result, event) = observe(
        planned(OpenCodeService::Go, "glm-5.3-flash"),
        response("429 Too Many Requests", "text/plain", &body),
    )
    .await;
    assert_eq!(result.unwrap_err(), ProviderError::ResponseLimitExceeded);
    check(
        event,
        DebugStage::BodyFraming,
        DebugFinish::Absent,
        false,
        false,
        Some(ProviderError::ResponseLimitExceeded),
    );
}

#[tokio::test]
async fn qa_actual_go_and_zen_transport_metadata() {
    for (service, model, logged) in [
        (OpenCodeService::Go, "glm-5.3-flash", DebugService::Go),
        (OpenCodeService::Zen, "minimax-m3", DebugService::Zen),
    ] {
        let request = planned(service, model);
        assert_eq!(request.maximum_output_tokens(), 8192);
        assert_eq!(request.model().maximum_output_tokens, 32000);
        let mut value = wire("stop");
        value["model"] = json!(model);
        let (result, event) = observe(request, response_for(&value, true)).await;
        assert!(result.is_ok());
        assert_eq!(
            event,
            DebugEvent::Provider {
                attempt_id: 41,
                service: logged,
                protocol: 2,
                requested_output_tokens: 8192,
                stage: DebugStage::Complete,
                finish: DebugFinish::Stop,
                done: true,
                usage_seen: true,
                receipt_accepted: true,
                error: None
            }
        );
    }
}

#[tokio::test]
async fn qa_terminal_failure_categories_and_missing_receipts() {
    for (reason, finish, error) in [
        (
            "length",
            DebugFinish::Length,
            ProviderError::IncompleteResponse,
        ),
        (
            "model_context_window_exceeded",
            DebugFinish::ContextWindowExceeded,
            ProviderError::IncompleteResponse,
        ),
        (
            "content_filter",
            DebugFinish::ContentFilter,
            ProviderError::ProviderExecutionFailed,
        ),
        (
            "SYNTHETIC_HOSTILE",
            DebugFinish::Other,
            ProviderError::MalformedResponse,
        ),
    ] {
        let (result, event) = observe(
            planned(OpenCodeService::Go, "glm-5.3-flash"),
            response_for(&wire(reason), true),
        )
        .await;
        assert_eq!(result.err(), Some(error));
        check(
            event,
            DebugStage::FinishReason,
            finish,
            true,
            true,
            Some(error),
        );
    }
    let (result, event) = observe(
        planned(OpenCodeService::Go, "glm-5.3-flash"),
        response_for(&wire("stop"), false),
    )
    .await;
    assert_eq!(result.err(), Some(ProviderError::IncompleteResponse));
    check(
        event,
        DebugStage::Termination,
        DebugFinish::Stop,
        false,
        true,
        Some(ProviderError::IncompleteResponse),
    );
}

#[tokio::test]
async fn qa_push_failure_snapshot_is_not_overwritten_by_finish() {
    let mut identity = wire("stop");
    identity["model"] = json!("SYNTHETIC_HOSTILE");
    let mut usage = wire("stop");
    usage["usage"]["total_tokens"] = json!(9);
    for (bytes, stage, finish, seen) in [
        (
            response_for(&identity, true),
            DebugStage::Identity,
            DebugFinish::Absent,
            true,
        ),
        (
            response_for(&usage, true),
            DebugStage::Usage,
            DebugFinish::Stop,
            true,
        ),
        (
            response(
                "200 OK",
                "text/event-stream",
                b"data: {SYNTHETIC_HOSTILE\n\n",
            ),
            DebugStage::Json,
            DebugFinish::Absent,
            false,
        ),
    ] {
        let (result, event) = observe(planned(OpenCodeService::Go, "glm-5.3-flash"), bytes).await;
        assert_eq!(result.err(), Some(ProviderError::MalformedResponse));
        check(
            event,
            stage,
            finish,
            false,
            seen,
            Some(ProviderError::MalformedResponse),
        );
    }
}

#[tokio::test]
async fn qa_final_tool_argument_json_snapshot_survives() {
    let mut value = wire("tool_calls");
    value["choices"][0]["delta"] = json!({"role":"assistant","tool_calls":[{"index":0,"id":"synthetic_call","type":"function","function":{"name":"read","arguments":"{"}}]});
    let (result, event) = observe(
        planned(OpenCodeService::Go, "glm-5.3-flash"),
        response_for(&value, true),
    )
    .await;
    assert_eq!(result.err(), Some(ProviderError::MalformedResponse));
    check(
        event,
        DebugStage::ArgumentJson,
        DebugFinish::ToolCalls,
        true,
        true,
        Some(ProviderError::MalformedResponse),
    );
}

#[tokio::test]
async fn qa_transport_rejections_have_no_receipt_or_reflection() {
    for (bytes, stage, error) in [
        (
            response("200 OK", "text/html", b"SYNTHETIC_HOSTILE"),
            DebugStage::ContentType,
            ProviderError::UnexpectedContentType,
        ),
        (
            response("302 Found", "text/plain", b"SYNTHETIC_HOSTILE"),
            DebugStage::HttpStatus,
            ProviderError::RedirectDenied,
        ),
    ] {
        let (result, event) = observe(planned(OpenCodeService::Go, "glm-5.3-flash"), bytes).await;
        assert_eq!(result.err(), Some(error));
        check(event, stage, DebugFinish::Absent, false, false, Some(error));
    }
}

#[tokio::test]
async fn qa_pre_cancelled_does_not_connect_or_allocate_a_global_id() {
    assert!(!crate::debug_log::enabled());
    assert_eq!(crate::debug_log::next_attempt_id(), None);
    let request = planned(OpenCodeService::Go, "glm-5.3-flash");
    let client = OpenCodeClient::for_test(endpoints("http://127.0.0.1:9"));
    let (handle, mut cancellation) = provider_cancellation();
    handle.cancel();
    let mut diagnostic = TransportDiagnostic::new(&request);
    let result = client
        .send_inference(
            TEST_KEY.as_bytes(),
            &request,
            request.encoded_body(),
            &mut cancellation,
            &mut diagnostic,
        )
        .await;
    assert!(matches!(result, Err(ProviderError::Cancelled)));
    let outcome: Result<ProviderOutcome, ProviderError> = Err(ProviderError::Cancelled);
    assert!(diagnostic.event(None, &outcome).is_none());
    check(
        diagnostic.event(Some(41), &outcome).unwrap(),
        DebugStage::Preparing,
        DebugFinish::Absent,
        false,
        false,
        Some(ProviderError::Cancelled),
    );
}
