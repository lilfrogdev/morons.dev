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
fn citation_reasons_identify_rejections_without_response_data() {
    use crate::debug_log::DebugCitationRejection::*;
    type Mutation = fn(&mut Value);
    let cases: &[(_, Mutation)] = &[
        (Annotations, |p| p["annotations"] = json!("PRIVATE")),
        (AnnotationType, |p| {
            p["annotations"][0]["type"] = json!("PRIVATE")
        }),
        (Url, |p| {
            p["annotations"][0]["url"] = json!("file:///PRIVATE")
        }),
        (Title, |p| p["annotations"][0]["title"] = json!(null)),
        (Offsets, |p| p["annotations"][0]["start_index"] = json!(-1)),
        (TitleLimit, |p| {
            p["annotations"][0]["title"] = json!("X".repeat(513))
        }),
        (OffsetOrder, |p| {
            p["annotations"][0]["start_index"] = json!(2);
            p["annotations"][0]["end_index"] = json!(1);
        }),
        (OffsetBounds, |p| {
            p["annotations"][0]["end_index"] = json!(99999)
        }),
        (MissingCitations, |p| p["annotations"] = json!([])),
    ];
    for (expected, mutate) in cases {
        let mut v = events();
        mutate(&mut v[2]["item"]["content"][0]);
        let mut stage = WebStage::Admission;
        let mut reason = None;
        let body = wire(&v);
        assert_eq!(
            super::super::decode::decode_response_with_citations(
                &body,
                &mut stage,
                &mut None,
                &mut reason
            ),
            Err(ProviderError::MalformedResponse)
        );
        assert_eq!(stage, WebStage::Citation);
        assert_eq!(reason, Some(*expected));
        assert_eq!(
            decode_response(&body),
            Err(ProviderError::MalformedResponse)
        );
    }
    let mut reason = Some(MissingCitations);
    super::super::decode::decode_response_with_citations(
        &response_fixture(),
        &mut WebStage::Admission,
        &mut None,
        &mut reason,
    )
    .unwrap();
    assert_eq!(reason, None);
}

#[test]
fn streamed_citation_reasons_distinguish_metadata_annotation_and_consistency() {
    use crate::debug_log::DebugCitationRejection::*;
    for expected in [StreamMetadata, StreamAnnotation, StreamConsistency] {
        let mut data = events();
        let annotation = data[2]["item"]["content"][0]["annotations"][0].clone();
        let mut event = json!({"type":"response.output_text.annotation.added","output_index":1,"content_index":0,"annotation_index":0,"item_id":"msg_fixture","annotation":annotation});
        match expected {
            StreamMetadata => event["content_index"] = json!(-1),
            StreamAnnotation => event["annotation"]["type"] = json!("PRIVATE"),
            StreamConsistency => event["annotation"]["url"] = json!("https://example.com/PRIVATE"),
            _ => unreachable!(),
        }
        data.insert(2, event);
        let mut stage = WebStage::Admission;
        let mut reason = None;
        assert_eq!(
            super::super::decode::decode_response_with_citations(
                &wire(&data),
                &mut stage,
                &mut None,
                &mut reason
            ),
            Err(ProviderError::MalformedResponse)
        );
        assert_eq!(stage, WebStage::Citation);
        assert_eq!(reason, Some(expected));
    }
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
