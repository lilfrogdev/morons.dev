use super::*;
use crate::web_diagnostic::{WebCategory, WebStage};

fn rejection(body: &[u8], expected: WebStage) {
    let mut stage = WebStage::Admission;
    let error = super::super::decode::decode_response_at(body, &mut stage).unwrap_err();
    assert_eq!(stage, expected);
    assert_eq!(decode_response(body).unwrap_err(), error);
    let failure = super::super::diagnostic::failure(stage, error);
    let text = format!(
        "{failure:?} {} {}",
        failure.stage.label(),
        failure.category.label()
    );
    assert!(!text.contains("PRIVATE"));
    assert!(text.len() < 256);
}

#[test]
fn hosted_diagnostic_tracks_exact_validation_guards_without_changing_errors() {
    type Mutation = fn(&mut Vec<Value>);
    let cases: &[(WebStage, Mutation)] = &[
        (WebStage::Lifecycle, |v| {
            v[0]["response"]["status"] = json!("PRIVATE")
        }),
        (WebStage::ResponseIdentity, |v| {
            v[3]["response"]["id"] = json!("resp_PRIVATE")
        }),
        (WebStage::ResponseModel, |v| {
            v[0]["response"]["model"] = json!("PRIVATE")
        }),
        (WebStage::ResponseModel, |v| {
            v[3]["response"]["model"] = json!("PRIVATE")
        }),
        (WebStage::EventKind, |v| v[1]["type"] = json!("PRIVATE")),
        (WebStage::OutputItem, |v| v[2]["output_index"] = json!(128)),
        (WebStage::OutputConsistency, |v| {
            v[2]["output_index"] = json!(2)
        }),
        (WebStage::OutputConsistency, |v| {
            v[3]["response"]["output"] = json!([item_call(), item_message()]);
            v[3]["response"]["output"][1]["content"][0]["text"] = json!("PRIVATE");
        }),
        (WebStage::Usage, |v| {
            v[3]["response"]["usage"]["total_tokens"] = json!(999)
        }),
        (WebStage::SearchAction, |v| {
            v[1]["item"]["action"]["type"] = json!("PRIVATE")
        }),
        (WebStage::AssistantMessage, |v| {
            v[2]["item"]["role"] = json!("PRIVATE")
        }),
        (WebStage::Citation, |v| {
            v[2]["item"]["content"][0]["annotations"][0]["url"] = json!("file:///PRIVATE")
        }),
        (WebStage::Citation, |v| {
            v[2]["item"]["content"][0]["annotations"] = json!([])
        }),
        (WebStage::Completion, |v| {
            v[2]["item"]["phase"] = json!("commentary")
        }),
        (WebStage::Reasoning, |v| {
            v[1]["item"] = json!({"id":"rs_fixture","type":"reasoning","status":"PRIVATE"});
        }),
        (WebStage::Termination, |v| {
            v.pop();
        }),
    ];
    for (expected, mutate) in cases {
        let mut v = events();
        mutate(&mut v);
        rejection(&wire(&v), *expected);
    }
    let body = String::from_utf8(response_fixture()).unwrap();
    rejection(
        body.replace("\"sequence_number\":1", "\"sequence_number\":8")
            .as_bytes(),
        WebStage::Sequence,
    );
    rejection(
        body.replace("event: response.created", "event: PRIVATE")
            .as_bytes(),
        WebStage::EventEnvelope,
    );
    rejection(b"data: {\"PRIVATE\":\n\n", WebStage::Json);
    rejection(b"data: [DONE]\n\n", WebStage::Termination);
    rejection(b"PRIVATE", WebStage::Sse);
    rejection(&vec![0; MAX_RESPONSE_BYTES + 1], WebStage::BodyBounds);
    let mut stage = WebStage::Admission;
    assert_eq!(
        super::super::decode::decode_response_at(&response_fixture(), &mut stage).unwrap(),
        decode_response(&response_fixture()).unwrap()
    );
}

#[test]
fn hosted_diagnostic_categories_preserve_existing_closed_provider_errors() {
    let cases = [
        (ProviderError::RequestRejected, WebCategory::RequestRejected),
        (
            ProviderError::AuthenticationOrEntitlement,
            WebCategory::AuthenticationOrEntitlement,
        ),
        (ProviderError::RateLimited, WebCategory::RateLimited),
        (
            ProviderError::ResponseHeaderTimeout,
            WebCategory::ResponseHeaderTimeout,
        ),
        (
            ProviderError::StreamInactivityTimeout,
            WebCategory::StreamInactivityTimeout,
        ),
        (ProviderError::TotalTimeout, WebCategory::TotalTimeout),
        (ProviderError::Cancelled, WebCategory::Cancelled),
        (ProviderError::Transport, WebCategory::Transport),
        (
            ProviderError::ResponseLimitExceeded,
            WebCategory::ResponseLimitExceeded,
        ),
    ];
    for (error, category) in cases {
        let failure = super::super::diagnostic::failure(WebStage::Headers, error);
        assert_eq!(failure.category, category);
        assert_eq!(failure.stage, WebStage::Headers);
    }
}
