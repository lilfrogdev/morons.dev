use crate::{
    debug_log::DebugNormalizationStage as Stage,
    persistence::RunFailureKind,
    provider::{ProviderInputItem, ProviderMessageRole},
};

#[derive(Default)]
pub(super) struct ChildRecovery {
    corrected: bool,
}

impl ChildRecovery {
    pub(super) fn correct(&mut self, failure: RunFailureKind, stage: Stage) -> bool {
        let allowed = matches!(
            stage,
            Stage::ArgumentJson
                | Stage::ToolFieldSet
                | Stage::ToolType
                | Stage::ReadWindow
                | Stage::EditBounds
                | Stage::Path
                | Stage::FinalMessageMissing
                | Stage::FinalMessageEmpty
                | Stage::FinalMessageMultiple
                | Stage::ToolTurnMessage
        );
        if self.corrected || failure != RunFailureKind::InvalidProviderOutput || !allowed {
            return false;
        }
        self.corrected = true;
        true
    }
}

pub(super) fn feedback(text: &str) -> ProviderInputItem {
    ProviderInputItem::Message {
        role: ProviderMessageRole::Developer,
        text: text.to_owned(),
        phase: None,
    }
}

pub(super) fn normalization_diagnostic(failure: RunFailureKind, stage: Stage) -> String {
    let reason = match stage {
        Stage::ArgumentJson => "tool arguments are not valid strict JSON",
        Stage::ToolFieldSet => "tool argument fields do not match the schema",
        Stage::ToolType => "tool argument has an invalid type",
        Stage::ReadWindow => "read offset or limit is outside the allowed window",
        Stage::EditBounds => "edit replacements violate bounds",
        Stage::Path => "tool path is invalid",
        Stage::FinalMessageMissing => "final assistant report is missing",
        Stage::FinalMessageEmpty => "final assistant report is empty",
        Stage::FinalMessageMultiple => "multiple final assistant messages",
        Stage::ToolTurnMessage => "invalid assistant message placement in tool response",
        Stage::Catalog => "tool catalog mismatch",
        Stage::CallCount => "invalid tool call count",
        Stage::DuplicateCallId => "duplicate tool call identifier",
        Stage::UnknownTool => "unknown tool",
        Stage::ForbiddenChildTool => "tool is forbidden for children",
        Stage::TaskBounds => "task bounds violated",
        Stage::ToolInFinal => "tool call in final response",
        Stage::OutputBytes => "response exceeds output bounds",
        Stage::Other => "unclassified response validation failure",
    };
    let label = if failure == RunFailureKind::ResourceLimit {
        "response exceeded limits"
    } else {
        "response rejected"
    };
    format!(
        "subagent {label}: {reason}; no tools from this response executed; earlier effects may remain"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correction_is_allowlisted_and_once_only() {
        let mut state = ChildRecovery::default();
        for stage in [
            Stage::DuplicateCallId,
            Stage::ForbiddenChildTool,
            Stage::Catalog,
            Stage::Other,
            Stage::OutputBytes,
        ] {
            assert!(!state.correct(RunFailureKind::InvalidProviderOutput, stage));
        }
        assert!(!state.correct(RunFailureKind::ResourceLimit, Stage::ReadWindow));
        assert!(state.correct(RunFailureKind::InvalidProviderOutput, Stage::ReadWindow));
        assert!(!state.correct(RunFailureKind::InvalidProviderOutput, Stage::ReadWindow));
    }
}
