use super::*;
use serde_json::json;

pub(crate) fn completed_item_stream_fixture(source: &str) -> String {
    let mut events: Vec<Value> = source
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|data| *data != "[DONE]")
        .map(|data| serde_json::from_str(data).unwrap())
        .collect();
    let mut terminal = events.pop().unwrap();
    let items = std::mem::take(terminal["response"]["output"].as_array_mut().unwrap());
    for (index, item) in items.into_iter().enumerate() {
        events.push(json!({"type":"response.output_item.done","sequence_number":events.len(),"output_index":index,"item":item}));
    }
    terminal["sequence_number"] = json!(events.len());
    events.push(terminal);
    events
        .into_iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect()
}
fn items() -> Vec<Value> {
    vec![
        json!({"id":"msg_fixture","type":"message","role":"assistant","status":"completed","phase":"final_answer","content":[{"type":"output_text","text":"fixture answer","annotations":[]}]}),
        json!({"id":"rs_fixture","type":"reasoning","summary":[],"encrypted_content":"PRIVATE-opaque"}),
        json!({"id":"fc_fixture","type":"function_call","status":"completed","call_id":"call_fixture","name":"read","arguments":"{}"}),
    ]
}
fn stream(done: bool, tail: bool) -> Vec<Value> {
    let mut events = vec![
        json!({"type":"response.created","sequence_number":0,"response":{"id":"resp_fixture","object":"response","status":"in_progress","model":"gpt-5.5"}}),
    ];
    if done {
        for (index, item) in items().into_iter().enumerate() {
            events.push(json!({"type":"response.output_item.done","sequence_number":events.len(),"output_index":index,"item":item}));
        }
    }
    events.push(json!({"type":"response.completed","sequence_number":events.len(),"response":{"id":"resp_fixture","object":"response","model":"gpt-5.5","status":"completed","output":if tail {items()} else {vec![]},"usage":{"input_tokens":10,"input_tokens_details":{"cached_tokens":2},"output_tokens":5,"output_tokens_details":{"reasoning_tokens":1},"total_tokens":15}}}));
    events
}
fn renumber(events: &mut [Value]) {
    for (index, event) in events.iter_mut().enumerate() {
        event["sequence_number"] = json!(index);
    }
}
fn decode(native: bool, events: Vec<Value>) -> Result<ProviderOutcome, ProviderError> {
    let mut decoder = if native {
        ResponsesDecoder::new_native("gpt-5.5", 96_000, 32_000)
    } else {
        ResponsesDecoder::new("gpt-5.5", 96_000, 32_000)
    };
    for event in events {
        decoder.push(format!("data: {event}\n\n").as_bytes())?;
    }
    decoder.finish()
}
#[test]
fn native_complete_item_events_supply_empty_terminal_output_without_changing_opencode() {
    let expected = decode(false, stream(false, true)).unwrap();
    assert_eq!(decode(true, stream(true, false)).unwrap(), expected);
    let mut extended = stream(true, true);
    extended[1]["item"]["ignored_extension"] = json!("PRIVATE");
    assert_eq!(decode(true, extended).unwrap(), expected);
    let mut reordered = stream(true, false);
    reordered.swap(1, 3);
    renumber(&mut reordered);
    assert_eq!(decode(true, reordered).unwrap(), expected);
    let mut refusal = stream(true, false);
    refusal[1]["item"]["content"] = json!([{"type":"refusal","refusal":"No."}]);
    assert!(
        matches!(&decode(true, refusal).unwrap().output[0], ProviderOutputItem::AssistantMessage(message) if message.refusal && message.text == "No.")
    );
    assert_eq!(decode(true, stream(true, true)).unwrap(), expected);
    assert_eq!(decode(true, stream(false, true)).unwrap(), expected);
    assert_eq!(
        decode(false, stream(true, false)).err(),
        Some(ProviderError::MalformedResponse)
    );
    assert_eq!(
        decode(true, stream(false, false)).err(),
        Some(ProviderError::MalformedResponse)
    );
}

#[test]
fn native_done_items_require_complete_consistent_scoped_terminal_evidence() {
    type Change = fn(&mut Vec<Value>);
    let cases: &[Change] = &[
        |v| {
            v.insert(2, v[1].clone());
        },
        |v| {
            v.remove(2);
        },
        |v| v[1]["output_index"] = json!(128),
        |v| v[1]["output_index"] = json!(null),
        |v| v[1]["item"] = json!(null),
        |v| v[1]["item"]["type"] = json!("PRIVATE"),
        |v| v[1]["item"]["role"] = json!("user"),
        |v| v[1]["item"]["status"] = json!("in_progress"),
        |v| v[2]["item"]["id"] = json!("msg_fixture"),
        |v| v[3]["item"]["arguments"] = json!("{"),
        |v| v[4]["response"]["id"] = json!("resp_foreign"),
        |v| v[4]["response"]["object"] = json!("PRIVATE"),
        |v| v[4]["response"]["status"] = json!("in_progress"),
        |v| v[4]["response"]["model"] = json!("PRIVATE"),
        |v| v[4]["response"]["usage"]["total_tokens"] = json!(20),
        |v| v[4]["response"]["output"] = json!(null),
        |v| {
            v[4]["response"].as_object_mut().unwrap().remove("output");
        },
        |v| {
            for item in &mut v[1..4] {
                item["type"] = json!("response.output_item.added");
            }
        },
        |v| {
            v[4]["response"]["output"] = json!(items());
            v[4]["response"]["output"][0]["content"][0]["text"] = json!("PRIVATE");
        },
        |v| {
            v[4]["response"]["output"] = json!(items());
            v[4]["response"]["output"][1]["encrypted_content"] = json!("PRIVATE-different");
        },
        |v| {
            v[4]["response"]["output"] = json!(items());
            v[4]["response"]["output"][2]["arguments"] = json!("{ }");
        },
    ];
    for change in cases {
        let mut events = stream(true, false);
        change(&mut events);
        renumber(&mut events);
        let error = decode(true, events).unwrap_err();
        assert_eq!(error, ProviderError::MalformedResponse);
        assert!(!format!("{error:?} {error}").contains("PRIVATE"));
    }
    let mut missing_terminal = stream(true, false);
    missing_terminal.pop();
    assert_eq!(
        decode(true, missing_terminal).err(),
        Some(ProviderError::IncompleteResponse)
    );
    // A new response cannot borrow complete items from the preceding decoder.
    assert_eq!(
        decode(true, stream(false, false)).err(),
        Some(ProviderError::MalformedResponse)
    );
    let mut wrong_sequence = stream(true, false);
    wrong_sequence[1]["sequence_number"] = json!(9);
    assert_eq!(
        decode(true, wrong_sequence).err(),
        Some(ProviderError::MalformedResponse)
    );
}

#[test]
fn native_done_items_reconcile_deltas_but_never_complete_from_deltas_alone() {
    let text = json!({"type":"response.output_text.delta","sequence_number":1,"output_index":0,"content_index":0,"item_id":"msg_fixture","delta":"fixture answer"});
    let arguments = json!({"type":"response.function_call_arguments.delta","sequence_number":2,"output_index":2,"item_id":"fc_fixture","delta":"{}"});
    let mut events = stream(true, false);
    events.insert(1, text.clone());
    events.insert(2, arguments.clone());
    renumber(&mut events);
    assert_eq!(
        decode(true, events.clone()).unwrap(),
        decode(false, stream(false, true)).unwrap()
    );
    for index in [1, 2] {
        let mut bad = events.clone();
        bad[index]["delta"] = json!("PRIVATE");
        assert_eq!(
            decode(true, bad).err(),
            Some(ProviderError::MalformedResponse)
        );
    }
    for mut delta in [text.clone(), arguments] {
        let mut late = stream(true, false);
        delta["delta"] = json!("");
        late.insert(4, delta);
        renumber(&mut late);
        assert_eq!(
            decode(true, late).err(),
            Some(ProviderError::MalformedResponse)
        );
    }
    let mut only_delta = stream(false, false);
    only_delta.insert(1, text);
    renumber(&mut only_delta);
    assert_eq!(
        decode(true, only_delta).err(),
        Some(ProviderError::MalformedResponse)
    );
}

#[test]
fn native_done_item_cache_has_aggregate_source_and_index_bounds() {
    use super::super::sse::MAX_SSE_RECORD_BYTES;
    let event = |index| json!({"output_index":index,"item":{"type":"message"}});
    let mut bytes = native::NativeCompletedItems::default();
    bytes.push(event(0), MAX_SSE_RECORD_BYTES, 1).unwrap();
    assert_eq!(
        bytes.push(event(1), 1, 1),
        Err(ProviderError::ResponseLimitExceeded)
    );
    let mut nodes = native::NativeCompletedItems::default();
    nodes.push(event(0), 1, MAX_EVENT_NODES).unwrap();
    assert_eq!(
        nodes.push(event(1), 1, 1),
        Err(ProviderError::ResponseLimitExceeded)
    );
    let mut items = native::NativeCompletedItems::default();
    for index in 0..MAX_OUTPUT_ITEMS {
        items.push(event(index), 1, 1).unwrap();
    }
    assert_eq!(
        items.push(event(MAX_OUTPUT_ITEMS), 1, 1),
        Err(ProviderError::MalformedResponse)
    );
    assert_eq!(items.finish().unwrap().unwrap().len(), MAX_OUTPUT_ITEMS);
}
