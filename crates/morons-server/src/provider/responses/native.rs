use super::*;
use serde::Deserialize;

pub(super) fn response_model_alias(expected_model: &str) -> Option<&'static str> {
    match expected_model {
        "gpt-daybreak-blue-latest" => Some("gpt-5.6-sol"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SELECTED: &str = "gpt-daybreak-blue-latest";
    const RESOLVED: &str = "gpt-5.6-sol";

    fn stream(models: [Option<&str>; 3]) -> [Value; 3] {
        let mut events = [
            json!({"type":"response.created","sequence_number":0,"response":{"id":"resp_alias","object":"response","status":"in_progress"}}),
            json!({"type":"response.in_progress","sequence_number":1,"response":{"id":"resp_alias","object":"response","status":"in_progress"}}),
            json!({"type":"response.completed","sequence_number":2,"response":{"id":"resp_alias","object":"response","status":"completed","output":[{"id":"msg_alias","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"ok","annotations":[]}]}],"usage":{"input_tokens":10,"input_tokens_details":{"cached_tokens":0},"output_tokens":5,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":15}}}),
        ];
        for (event, model) in events.iter_mut().zip(models) {
            if let Some(model) = model {
                event["response"]["model"] = json!(model);
            }
        }
        events
    }

    fn push(decoder: &mut ResponsesDecoder, event: &Value) -> Result<(), ProviderError> {
        decoder
            .push(format!("data: {event}\n\n").as_bytes())
            .map(|_| ())
    }

    #[test]
    fn native_alias_accepts_selected_resolved_and_optional_lifecycle_models() {
        for created in [None, Some(SELECTED), Some(RESOLVED)] {
            for progress in [None, Some(SELECTED), Some(RESOLVED)] {
                for completed in [SELECTED, RESOLVED] {
                    let mut decoder = ResponsesDecoder::new_native(SELECTED, 96_000, 32_000);
                    for event in stream([created, progress, Some(completed)]) {
                        push(&mut decoder, &event).unwrap();
                    }
                    assert_eq!(decoder.finish().unwrap().usage.total_tokens, 15);
                }
            }
        }
    }

    #[test]
    fn native_alias_rejects_unreviewed_names_at_every_model_guard() {
        for wrong in [
            "gpt-5.6-terra",
            "gpt-5.7-sol",
            "gpt-5.6-sol-2027-01-01",
            "gpt-5.6",
            "gpt-5.6-sol-extra",
            "Gpt-5.6-sol",
            "GPT-DAYBREAK-BLUE-LATEST",
            " gpt-5.6-sol",
            "gpt-5.6-sol ",
            "gpt-5.6-sol\n",
            "gpt-5.6-sol\u{0000}",
            "gpt-daybreak-blue-latest ",
            "gpt-daybreak-blue-latest-extra",
            "",
        ] {
            for index in 0..3 {
                let mut models = [Some(SELECTED); 3];
                models[index] = Some(wrong);
                let mut decoder = ResponsesDecoder::new_native(SELECTED, 96_000, 32_000);
                for (position, event) in stream(models).iter().enumerate().take(index + 1) {
                    let result = push(&mut decoder, event);
                    if position == index {
                        assert_eq!(
                            result,
                            Err(ProviderError::MalformedResponse),
                            "{wrong:?} at {index}"
                        );
                        assert_eq!(decoder.failure_stage(), ResponseStage::ResponseModel);
                    } else {
                        result.unwrap();
                    }
                }
            }
        }
    }

    #[test]
    fn native_alias_is_directional_and_native_only() {
        for (native, expected, returned) in [
            (true, RESOLVED, SELECTED),
            (true, "gpt-5.5", RESOLVED),
            (true, "gpt-6-astra", RESOLVED),
            (true, "gpt-5.6-luna", RESOLVED),
            (true, "gpt-5.6-terra", RESOLVED),
            (false, SELECTED, RESOLVED),
            (false, RESOLVED, SELECTED),
        ] {
            for index in 0..3 {
                let mut decoder = if native {
                    ResponsesDecoder::new_native(expected, 96_000, 32_000)
                } else {
                    ResponsesDecoder::new(expected, 96_000, 32_000)
                };
                let mut models = [Some(expected); 3];
                models[index] = Some(returned);
                for event in stream(models).iter().take(index) {
                    push(&mut decoder, event).unwrap();
                }
                assert_eq!(
                    push(&mut decoder, &stream(models)[index]),
                    Err(ProviderError::MalformedResponse)
                );
                assert_eq!(decoder.failure_stage(), ResponseStage::ResponseModel);
            }
        }
    }

    #[test]
    fn native_alias_preserves_required_completed_model_shape_and_guard_order() {
        for shape in [
            None,
            Some(Value::Null),
            Some(json!(42)),
            Some(json!({})),
            Some(json!([])),
        ] {
            let mut stages = Vec::new();
            for model in [SELECTED, RESOLVED] {
                let mut decoder = ResponsesDecoder::new_native(SELECTED, 96_000, 32_000);
                let mut events = stream([Some(model); 3]);
                if let Some(shape) = &shape {
                    events[2]["response"]["model"] = shape.clone();
                } else {
                    events[2]["response"]
                        .as_object_mut()
                        .unwrap()
                        .remove("model");
                }
                push(&mut decoder, &events[0]).unwrap();
                push(&mut decoder, &events[1]).unwrap();
                assert_eq!(
                    push(&mut decoder, &events[2]),
                    Err(ProviderError::MalformedResponse)
                );
                assert_ne!(decoder.failure_stage(), ResponseStage::ResponseModel);
                stages.push(decoder.failure_stage());
            }
            assert_eq!(stages[0], stages[1]);
        }
        let mut decoder = ResponsesDecoder::new_native(SELECTED, 96_000, 32_000);
        let mut events = stream([Some("wrong"); 3]);
        events[0]["response"]["object"] = json!("wrong");
        assert_eq!(
            push(&mut decoder, &events[0]),
            Err(ProviderError::MalformedResponse)
        );
        assert_eq!(decoder.failure_stage(), ResponseStage::ResponseIdentity);
    }
}

/// One response's complete native items, never a cross-request continuation.
#[derive(Default)]
pub(super) struct NativeCompletedItems {
    items: BTreeMap<u32, Value>,
    source_bytes: usize,
    source_nodes: usize,
}
#[derive(Deserialize)]
struct DoneEvent {
    output_index: u32,
    item: Value,
}
impl NativeCompletedItems {
    pub(super) fn contains(&self, index: u32) -> bool {
        self.items.contains_key(&index)
    }
    pub(super) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    pub(super) fn push(
        &mut self,
        value: Value,
        bytes: usize,
        nodes: usize,
    ) -> Result<(), ProviderError> {
        let event: DoneEvent =
            serde_json::from_value(value).map_err(|_| ProviderError::MalformedResponse)?;
        if event.output_index as usize >= MAX_OUTPUT_ITEMS
            || !event.item.is_object()
            || self.items.contains_key(&event.output_index)
        {
            return Err(ProviderError::MalformedResponse);
        }
        self.source_bytes = self
            .source_bytes
            .checked_add(bytes)
            .filter(|n| *n <= super::super::sse::MAX_SSE_RECORD_BYTES)
            .ok_or(ProviderError::ResponseLimitExceeded)?;
        self.source_nodes = self
            .source_nodes
            .checked_add(nodes)
            .filter(|n| *n <= MAX_EVENT_NODES)
            .ok_or(ProviderError::ResponseLimitExceeded)?;
        self.items.insert(event.output_index, event.item);
        Ok(())
    }
    pub(super) fn finish(self) -> Result<Option<Vec<Value>>, ProviderError> {
        if self.items.is_empty() {
            return Ok(None);
        }
        if self
            .items
            .keys()
            .enumerate()
            .any(|(expected, index)| expected != *index as usize)
        {
            return Err(ProviderError::MalformedResponse);
        }
        Ok(Some(self.items.into_values().collect()))
    }
}
