use super::*;
use serde_json::{Value, json};

fn item_call() -> Value {
    json!({"id":"ws_fixture","type":"web_search_call","status":"completed","action":{"type":"search","queries":["fixture"],"sources":[{"type":"url","url":"https://example.com/source"}]}})
}
fn item_message() -> Value {
    json!({"id":"msg_fixture","type":"message","role":"assistant","status":"completed","phase":"final_answer","content":[{"type":"output_text","text":"Fixture answer.","annotations":[{"type":"url_citation","url":"https://example.com/source","title":"Fixture source","start_index":0,"end_index":7}]}]})
}
pub(super) fn response_fixture() -> Vec<u8> {
    wire(&events())
}
fn events() -> Vec<Value> {
    vec![
        json!({"type":"response.created","response":{"id":"resp_fixture","object":"response","model":MODEL,"status":"in_progress"}}),
        json!({"type":"response.output_item.done","output_index":0,"item":item_call()}),
        json!({"type":"response.output_item.done","output_index":1,"item":item_message()}),
        json!({"type":"response.completed","response":{"id":"resp_fixture","object":"response","model":MODEL,"status":"completed","output":[],"usage":{"input_tokens":12,"input_tokens_details":{"cached_tokens":2},"output_tokens":4,"output_tokens_details":{"reasoning_tokens":1},"total_tokens":16}}}),
    ]
}
fn wire(events: &[Value]) -> Vec<u8> {
    let mut body = Vec::new();
    for (sequence, original) in events.iter().enumerate() {
        let mut value = original.clone();
        value["sequence_number"] = json!(sequence);
        body.extend_from_slice(
            format!(
                "event: {}\ndata: {}\n\n",
                value["type"].as_str().unwrap(),
                value
            )
            .as_bytes(),
        );
    }
    body
}
#[test]
fn hosted_request_is_exact_isolated_and_policy_checked_without_transport() {
    let request = SearchRequest::new("PRIVATE query", DataUseRestrictions::default()).unwrap();
    let value: Value = serde_json::from_slice(request.body()).unwrap();
    assert_eq!(value["model"], MODEL);
    assert_eq!(CONTRACT_REVISION, 1);
    assert_eq!(
        value["tools"],
        json!([{"type":"web_search","external_web_access":true,"search_context_size":"low"}])
    );
    assert_eq!(value["tool_choice"], json!({"type":"web_search"}));
    assert_eq!(
        value["input"],
        json!([{"type":"message","role":"user","content":[{"type":"input_text","text":"PRIVATE query"}]}])
    );
    for field in [
        "max_output_tokens",
        "previous_response_id",
        "metadata",
        "access_programs",
        "prompt_cache_key",
    ] {
        assert!(value.get(field).is_none());
    }
    assert!(!format!("{request:?}").contains("PRIVATE"));
    for flags in [(true, false), (false, true), (true, true)] {
        assert!(matches!(
            SearchRequest::new(
                "fixture",
                DataUseRestrictions {
                    block_training_use: flags.0,
                    require_zero_retention: flags.1
                }
            ),
            Err(ProviderError::DataUseRestricted)
        ));
    }
    for query in [
        "".to_owned(),
        " ".into(),
        "x".repeat(513),
        "bad\nquery".into(),
    ] {
        assert!(SearchRequest::new(&query, DataUseRestrictions::default()).is_err());
    }
    assert!(SearchRequest::new(&"x".repeat(512), DataUseRestrictions::default()).is_ok());
}

#[test]
fn hosted_complete_items_and_terminal_evidence_produce_cited_result_only_here() {
    let body = wire(&events());
    let result = decode_response(&body).unwrap();
    assert_eq!(result.answer, "Fixture answer.");
    assert_eq!(result.citations.len(), 1);
    assert_eq!(result.search_calls, 1);
    assert_eq!(result.usage.total_tokens, 16);
    let mut full = events();
    full[3]["response"]["output"] = json!([item_call(), item_message()]);
    assert_eq!(decode_response(&wire(&full)).unwrap(), result);
    full.remove(2);
    full.remove(1);
    assert_eq!(decode_response(&wire(&full)).unwrap(), result);
    let mut reordered = events();
    reordered.swap(1, 2);
    assert_eq!(decode_response(&wire(&reordered)).unwrap(), result);
    let mut done = body.clone();
    done.extend_from_slice(b"data: [DONE]\n\n");
    assert_eq!(decode_response(&done).unwrap(), result);
    let mut coding =
        crate::provider::responses::ResponsesDecoder::new_native(MODEL, 96_000, 32_000);
    assert!(coding.push(&body).is_err() || coding.finish().is_err());
    assert!(!format!("{result:?}").contains("Fixture answer"));
    assert!(!format!("{:?}", result.citations[0]).contains("example.com"));
}

#[test]
fn hosted_actions_and_unicode_citations_are_bounded_metadata_not_local_navigation() {
    let mut data = events();
    data.insert(2,json!({"type":"response.output_item.done","output_index":1,"item":{"id":"ws_open","type":"web_search_call","status":"completed","action":{"type":"open_page","url":"https://example.com/source#part"}}}));
    data.insert(3,json!({"type":"response.output_item.done","output_index":2,"item":{"id":"ws_find","type":"web_search_call","status":"completed","action":{"type":"find_in_page","url":"https://example.com/source","pattern":"needle"}}}));
    data[4]["output_index"] = json!(3);
    data[4]["item"]["content"][0]["text"] = json!("🦀 café source");
    let result = decode_response(&wire(&data)).unwrap();
    assert_eq!(
        (
            result.search_calls,
            result.open_page_calls,
            result.find_in_page_calls
        ),
        (1, 1, 1)
    );
    assert_eq!(result.answer, "🦀 café source");
    for url in [
        "javascript:alert(1)",
        "file:///private",
        "https://user:password@example.com",
        "https://example.com/\nsecret",
        "https://%40example.com",
        "//example.com",
    ] {
        let mut bad = events();
        bad[2]["item"]["content"][0]["annotations"][0]["url"] = json!(url);
        assert!(decode_response(&wire(&bad)).is_err());
    }
}

#[test]
fn hosted_output_rejects_missing_search_citations_and_contradictions() {
    let mutations: [fn(&mut Vec<Value>); 16] = [
        |v| {
            v[1]["item"]["type"] = json!("function_call");
        },
        |v| {
            v[1]["item"]["status"] = json!("in_progress");
        },
        |v| {
            v[1]["item"]["action"]["type"] = json!("unknown");
        },
        |v| {
            v[1]["item"]["action"]["queries"] = json!([null]);
        },
        |v| {
            v[2]["item"]["content"][0]["annotations"] = json!([]);
        },
        |v| {
            v[2]["item"]["role"] = json!("user");
        },
        |v| {
            v[2]["item"]["phase"] = json!("commentary");
        },
        |v| {
            v[2]["item"]["content"][0]["annotations"][0]["end_index"] = json!(10000);
        },
        |v| {
            v[2]["item"]["content"][0]["annotations"][0]["type"] = json!("file_citation");
        },
        |v| {
            v[3]["response"]["model"] = json!("gpt-5.6-sol");
        },
        |v| {
            v[3]["response"]["id"] = json!("resp_other");
        },
        |v| {
            v[3]["response"]["usage"]["total_tokens"] = json!(15);
        },
        |v| {
            v[3]["response"]["usage"]["input_tokens_details"]["cached_tokens"] = json!(13);
        },
        |v| {
            v[3]["response"]["output"] = Value::Null;
        },
        |v| {
            v[3]["response"]["output"] = json!([item_call(), item_message()]);
            v[3]["response"]["output"][1]["content"][0]["text"] = json!("contradictory");
        },
        |v| {
            v[2]["item"]["id"] = json!("ws_fixture");
        },
    ];
    for (i, change) in mutations.iter().enumerate() {
        let mut value = events();
        change(&mut value);
        assert!(decode_response(&wire(&value)).is_err(), "case {i}");
    }
    let mut no_search = events();
    no_search.remove(1);
    no_search[1]["output_index"] = json!(0);
    assert!(decode_response(&wire(&no_search)).is_err());
}

#[test]
fn hosted_stream_requires_scoped_ordered_complete_evidence() {
    for mutate in [
        |v: &mut Vec<Value>| {
            v.remove(0);
        },
        |v: &mut Vec<Value>| {
            v.pop();
        },
        |v: &mut Vec<Value>| {
            v[2]["output_index"] = json!(0);
        },
        |v: &mut Vec<Value>| {
            v[2]["output_index"] = json!(2);
        },
        |v: &mut Vec<Value>| {
            v[2]["output_index"] = json!(128);
        },
        |v: &mut Vec<Value>| {
            v[1]["type"] = json!("response.output_item.added");
            v[2]["type"] = json!("response.output_item.added");
        },
        |v: &mut Vec<Value>| {
            v.push(v[3].clone());
        },
    ] {
        let mut value = events();
        mutate(&mut value);
        assert!(decode_response(&wire(&value)).is_err());
    }
    let body = String::from_utf8(wire(&events())).unwrap();
    for bad in [
        body.replace("\"sequence_number\":1", "\"sequence_number\":9"),
        body.replace(
            "\"sequence_number\":1",
            "\"sequence_number\":1,\"sequence_number\":1",
        ),
        body.replace("event: response.created", "event: PRIVATE"),
    ] {
        assert!(decode_response(bad.as_bytes()).is_err());
    }
    assert!(decode_response(b"<html>PRIVATE</html>").is_err());
    let mut partial = wire(&events());
    partial.pop();
    assert!(decode_response(&partial).is_err());
}

#[test]
fn hosted_deltas_cannot_fabricate_or_contradict_complete_output() {
    let delta = json!({"type":"response.output_text.delta","output_index":1,"content_index":0,"item_id":"msg_fixture","delta":"Fixture answer."});
    let mut data = events();
    data.insert(2, delta.clone());
    assert!(decode_response(&wire(&data)).is_ok());
    data[2]["delta"] = json!("PRIVATE mismatch");
    assert!(decode_response(&wire(&data)).is_err());
    let mut late = events();
    late.insert(3, delta);
    assert!(decode_response(&wire(&late)).is_err());
    let mut reasoning = events();
    reasoning.insert(1,json!({"type":"response.output_item.done","output_index":2,"item":{"id":"rs_fixture","type":"reasoning","summary":[{"type":"summary_text","text":"PRIVATE reasoning"}],"encrypted_content":"PRIVATE opaque"}}));
    let result = decode_response(&wire(&reasoning)).unwrap();
    assert!(!result.answer.contains("PRIVATE"));
    assert!(!format!("{result:?}").contains("PRIVATE"));
}

#[test]
fn hosted_annotation_progress_must_match_complete_citations() {
    let mut data = events();
    let annotation = data[2]["item"]["content"][0]["annotations"][0].clone();
    let event = json!({"type":"response.output_text.annotation.added","output_index":1,"content_index":0,"annotation_index":0,"item_id":"msg_fixture","annotation":annotation});
    data.insert(2, event.clone());
    data[0]["response"]["model"] = Value::Null;
    data[1]["item"]["action"]["query"] = Value::Null;
    assert!(decode_response(&wire(&data)).is_ok());
    data[2]["annotation"]["url"] = json!("https://example.com/contradiction");
    assert!(decode_response(&wire(&data)).is_err());
    data[2]["annotation"] = Value::Null;
    assert!(decode_response(&wire(&data)).is_ok());
    let mut late = events();
    late.insert(3, event.clone());
    assert!(decode_response(&wire(&late)).is_err());
    let mut duplicate = events();
    duplicate.insert(2, event.clone());
    duplicate.insert(3, event);
    assert!(decode_response(&wire(&duplicate)).is_err());
}

#[test]
fn hosted_source_answer_action_and_citation_limits_fail_closed() {
    assert!(matches!(
        decode_response(&vec![b' '; MAX_RESPONSE_BYTES + 1]),
        Err(ProviderError::ResponseLimitExceeded)
    ));
    let mut data = events();
    data[2]["item"]["content"][0]["text"] = json!("x".repeat(MAX_ANSWER_BYTES));
    assert!(decode_response(&wire(&data)).is_ok());
    data[2]["item"]["content"][0]["text"] = json!("x".repeat(MAX_ANSWER_BYTES + 1));
    assert!(decode_response(&wire(&data)).is_err());
    let mut data = events();
    let citation = data[2]["item"]["content"][0]["annotations"][0].clone();
    data[2]["item"]["content"][0]["annotations"] = json!(vec![citation; MAX_CITATIONS + 1]);
    assert!(decode_response(&wire(&data)).is_err());
    let mut data = events();
    data[2]["output_index"] = json!(9);
    for i in 1..9 {
        let mut item = item_call();
        item["id"] = json!(format!("ws_{i}"));
        data.insert(
            i + 1,
            json!({"type":"response.output_item.done","output_index":i,"item":item}),
        );
    }
    assert!(decode_response(&wire(&data)).is_err());
}
