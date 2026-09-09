use super::*;

pub(super) fn sources(count: usize) -> Value {
    json!(
        (0..count)
            .map(|i| json!({"type":"url","url":format!("https://example.com/consulted/{i}")}))
            .collect::<Vec<_>>()
    )
}

#[test]
fn consulted_sources_are_not_limited_by_final_answer_citation_count() {
    let mut data = events();
    data[1]["item"]["action"]["sources"] = sources(11);
    let result = decode_response(&wire(&data))
        .expect("eleven consulted URLs are not eleven answer citations");
    assert_eq!(result.citations.len(), 1);
    assert_eq!(result.search_calls, 1);
    assert_eq!(result.answer, "Fixture answer.");
    data[3]["response"]["output"] = json!([data[1]["item"].clone(), data[2]["item"].clone()]);
    assert_eq!(decode_response(&wire(&data)).unwrap(), result);
}

#[test]
fn consulted_source_aggregate_and_other_limits_remain_independent() {
    use crate::web_diagnostic::WebStage;
    fn limited(data: &[Value], expected: WebStage) {
        let mut stage = WebStage::Admission;
        assert_eq!(
            super::super::decode::decode_response_at(&wire(data), &mut stage).unwrap_err(),
            ProviderError::ResponseLimitExceeded
        );
        assert_eq!(stage, expected);
    }
    let mut data = events();
    data[1]["item"]["action"]["sources"] = sources(128);
    assert_eq!(decode_response(&wire(&data)).unwrap().citations.len(), 1);
    data[1]["item"]["action"]["sources"] = sources(129);
    limited(&data, WebStage::SearchSources);

    let mut data = events();
    data[1]["item"]["action"]["sources"] = sources(64);
    let mut second = item_call();
    second["id"] = json!("ws_extra");
    second["action"]["sources"] = sources(64);
    data.insert(
        2,
        json!({"type":"response.output_item.done","output_index":1,"item":second}),
    );
    data[3]["output_index"] = json!(2);
    assert_eq!(decode_response(&wire(&data)).unwrap().search_calls, 2);
    data[2]["item"]["action"]["sources"] = sources(65);
    limited(&data, WebStage::SearchSources);

    let mut data = events();
    data[1]["item"]["action"]["queries"] = json!(vec!["query"; 9]);
    limited(&data, WebStage::SearchQueries);
    data[1]["item"]["action"]["queries"] = json!(vec!["query"; 8]);
    assert!(decode_response(&wire(&data)).is_ok());
    let annotation = data[2]["item"]["content"][0]["annotations"][0].clone();
    data[2]["item"]["content"][0]["annotations"] = json!(vec![annotation; 11]);
    limited(&data, WebStage::Citation);

    let mut data = events();
    data[2]["output_index"] = json!(9);
    for i in 1..9 {
        let mut item = item_call();
        item["id"] = json!(format!("ws_extra_{i}"));
        data.insert(
            i + 1,
            json!({"type":"response.output_item.done","output_index":i,"item":item}),
        );
    }
    limited(&data, WebStage::SearchActionCount);
    let mut data = events();
    data[1]["item"]["action"]["sources"] = sources(128);
    data[1]["item"]["action"]["sources"][0]["url"] = json!("file:///PRIVATE");
    assert!(decode_response(&wire(&data)).is_err());
    data[1]["item"]["action"]["sources"] = json!(vec![
        json!({"type":"url","url":format!("https://example.com/{}","x".repeat(4000))});
        128
    ]);
    limited(&data, WebStage::BodyBounds);
}
