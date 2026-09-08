use super::*;
use serde::Deserialize;

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
