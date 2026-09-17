use crate::{
    persistence::{ChildEntryKind, PersistenceError, SessionStore, ToolCallId},
    provider::{ProviderInputItem, ProviderOutcome, ProviderOutputItem},
};
use serde_json::{Value, json};

pub(super) struct Journal {
    call: ToolCallId,
    child: u16,
    ordinal: u64,
    digest: [u8; 32],
}

impl Journal {
    pub(super) fn new(call: [u8; 16], child: u16) -> Self {
        Self {
            call: ToolCallId::from_bytes(call),
            child,
            ordinal: 0,
            digest: [0; 32],
        }
    }

    pub(super) async fn append(
        &mut self,
        sessions: &SessionStore,
        kind: ChildEntryKind,
        payload: Value,
    ) -> Result<(), PersistenceError> {
        let payload = serde_json::to_vec(&payload).map_err(|_| invalid())?;
        let ordinal = self.ordinal.checked_add(1).ok_or_else(invalid)?;
        self.digest = sessions
            .append_child_entry(self.call, self.child, ordinal, kind, payload, self.digest)
            .await?;
        self.ordinal = ordinal;
        Ok(())
    }
}

pub(super) fn items(input: &[ProviderInputItem]) -> Value {
    Value::Array(input.iter().map(|item| match item {
        ProviderInputItem::Message {role,text,phase} => json!({"type":"message","role":role,"text":text,"phase":phase}),
        ProviderInputItem::FunctionCall {call_id,name,arguments,opaque_continuation} => json!({"type":"call","call_id":call_id,"name":name,"arguments":arguments,"opaque_continuation":opaque_continuation}),
        ProviderInputItem::FunctionCallOutput {call_id,output} => json!({"type":"result","call_id":call_id,"output":output}),
        ProviderInputItem::Reasoning {id,summaries,encrypted_content} => json!({"type":"reasoning","id":id,"summaries":summaries,"encrypted_content":encrypted_content}),
        ProviderInputItem::MultimodalMessage {..} => unreachable!("child images are rejected before entering context"),
    }).collect())
}

pub(super) fn outcome(outcome: &ProviderOutcome) -> Value {
    let output: Vec<Value> = outcome.output.iter().map(|item| match item {
        ProviderOutputItem::AssistantMessage(m) => json!({"type":"message","id":m.provider_item_id,"phase":m.phase,"text":m.text,"refusal":m.refusal}),
        ProviderOutputItem::ToolCall(c) => json!({"type":"call","id":c.provider_item_id,"call_id":c.provider_call_id,"name":c.name,"arguments":c.arguments,"opaque_continuation":c.opaque_continuation}),
        ProviderOutputItem::Reasoning(r) => json!({"type":"reasoning","id":r.provider_item_id,"summaries":r.summaries,"encrypted_content":r.encrypted_content}),
    }).collect();
    let u = outcome.usage;
    json!({"response_id":outcome.provider_response_id,"output":output,"usage":{
        "input_tokens":u.input_tokens,"output_tokens":u.output_tokens,"cached_input_tokens":u.cached_input_tokens,
        "cache_write_input_tokens":u.cache_write_input_tokens,"reasoning_output_tokens":u.reasoning_output_tokens,"total_tokens":u.total_tokens}})
}

fn invalid() -> PersistenceError {
    PersistenceError::InvalidState {
        reason: "child journal encoding or accounting failed",
    }
}
