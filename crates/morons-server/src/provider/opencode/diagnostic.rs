use super::{OpenCodeResponseRequest, OpenCodeService, ProviderError, ProviderOutcome};
use crate::{
    debug_log::{DebugEvent, DebugFinish, DebugService, DebugStage},
    provider::chat_completions::ChatDiagnosticSnapshot,
};

pub(super) struct TransportDiagnostic {
    service: DebugService,
    protocol: u16,
    requested_output_tokens: u32,
    snapshot: ChatDiagnosticSnapshot,
}

impl TransportDiagnostic {
    pub(super) fn new(request: &OpenCodeResponseRequest) -> Self {
        Self {
            service: match request.model().service {
                OpenCodeService::Zen => DebugService::Zen,
                OpenCodeService::Go => DebugService::Go,
            },
            protocol: request.model().protocol_revision,
            requested_output_tokens: request.maximum_output_tokens(),
            snapshot: ChatDiagnosticSnapshot {
                stage: DebugStage::Preparing,
                finish: DebugFinish::Absent,
                done: false,
                usage_seen: false,
            },
        }
    }

    pub(super) fn stage(&mut self, stage: DebugStage) {
        self.snapshot.stage = stage;
    }

    pub(super) fn snapshot(&mut self, snapshot: ChatDiagnosticSnapshot) {
        self.snapshot = snapshot;
    }

    pub(super) fn event(
        &self,
        attempt_id: Option<u64>,
        result: &Result<ProviderOutcome, ProviderError>,
    ) -> Option<DebugEvent> {
        Some(DebugEvent::Provider {
            attempt_id: attempt_id?,
            service: self.service,
            protocol: self.protocol,
            requested_output_tokens: self.requested_output_tokens,
            stage: self.snapshot.stage,
            finish: self.snapshot.finish,
            done: self.snapshot.done,
            usage_seen: self.snapshot.usage_seen,
            receipt_accepted: result.is_ok(),
            error: result.as_ref().err().copied(),
        })
    }
}
