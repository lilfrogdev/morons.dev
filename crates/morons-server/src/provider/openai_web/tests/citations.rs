use super::*;
use crate::debug_log::DebugCitationRejection;
use crate::web_diagnostic::WebStage;

fn reject(data: &[Value], expected: DebugCitationRejection) {
    let mut stage = WebStage::Admission;
    let mut reason = None;
    assert_eq!(
        super::super::decode::decode_response_with_citations(
            &wire(data),
            &mut stage,
            &mut None,
            &mut reason,
        ),
        Err(ProviderError::MalformedResponse)
    );
    assert_eq!(stage, WebStage::Citation);
    assert_eq!(reason, Some(expected));
}

#[test]
fn uncited_answers_do_not_promote_sources_or_inline_links_to_citations() {
    for text in [
        "Fixture answer.",
        "See [source](https://example.com/source).",
        "Fixture answer. citeturn0search0",
    ] {
        let mut data = events();
        data[2]["item"]["content"][0]["text"] = json!(text);
        data[2]["item"]["content"][0]["annotations"] = json!([]);
        let result = decode_response(&wire(&data)).unwrap();
        assert_eq!(result.answer, text);
        assert!(result.citations.is_empty());
        data[3]["response"]["output"] = json!([data[1]["item"].clone(), data[2]["item"].clone()]);
        data.drain(1..3);
        assert_eq!(decode_response(&wire(&data)).unwrap(), result);
    }
}

#[test]
fn commentary_citations_are_not_promoted_to_the_final_answer() {
    let mut data = events();
    let mut commentary = item_message();
    commentary["id"] = json!("msg_commentary");
    commentary["phase"] = json!("commentary");
    data[2]["output_index"] = json!(2);
    data[2]["item"]["content"][0]["annotations"] = json!([]);
    data.insert(
        2,
        json!({"type":"response.output_item.done","output_index":1,"item":commentary}),
    );
    assert!(decode_response(&wire(&data)).unwrap().citations.is_empty());
    data[3]["item"]["content"][0]["annotations"] =
        item_message()["content"][0]["annotations"].clone();
    let result = decode_response(&wire(&data)).unwrap();
    assert_eq!(result.citations.len(), 1);
    assert_eq!(result.answer, "Fixture answer.");
}

#[test]
fn malformed_annotation_shapes_are_rejected() {
    for (annotations, reason) in [
        (json!(null), DebugCitationRejection::Annotations),
        (json!({}), DebugCitationRejection::Annotations),
        (
            json!([{"type":"url_citation","url_citation":{
                "url":"https://example.com/source","title":"Source",
                "start_index":0,"end_index":7
            }}]),
            DebugCitationRejection::Url,
        ),
        (
            json!([{"type":"citation","url":"https://example.com/source"}]),
            DebugCitationRejection::AnnotationType,
        ),
    ] {
        let mut data = events();
        data[2]["item"]["content"][0]["annotations"] = annotations;
        reject(&data, reason);
    }
    let mut data = events();
    data[2]["item"]["content"][0]
        .as_object_mut()
        .unwrap()
        .remove("annotations");
    reject(&data, DebugCitationRejection::Annotations);
}

#[test]
fn streamed_citations_must_agree_with_completed_annotations() {
    let mut data = events();
    let annotation = data[2]["item"]["content"][0]["annotations"][0].clone();
    data.insert(
        2,
        json!({
            "type":"response.output_text.annotation.added","output_index":1,
            "content_index":0,"annotation_index":0,"item_id":"msg_fixture",
            "annotation":annotation
        }),
    );
    assert_eq!(
        decode_response(&wire(&data)).unwrap(),
        decode_response(&response_fixture()).unwrap()
    );
    data[3]["item"]["content"][0]["annotations"] = json!([]);
    reject(&data, DebugCitationRejection::StreamConsistency);
}
