use serde::Serialize;

use crate::{persistence::PersistenceResourceLimit, tools::ToolKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DebugLocation {
    Root {
        run_id: [u8; 16],
    },
    Child {
        session_id: [u8; 16],
        task_call_id: [u8; 16],
        child_index: u16,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugResource {
    Sessions,
    Runs,
    Context,
    Transcript,
    LogicalSequence,
    CredentialGeneration,
    CredentialMutations,
    ModelSelections,
    DataUsePolicies,
    RootDeadline,
    ChildProviderTurn,
    ChildContextEstimate,
    ChildContextBudget,
    ChildToolBudget,
    ChildUsageOverflow,
    ChildFinalOutput,
    NormalizedOutput,
    ProviderRequest,
}

impl From<PersistenceResourceLimit> for DebugResource {
    fn from(resource: PersistenceResourceLimit) -> Self {
        match resource {
            PersistenceResourceLimit::Sessions => Self::Sessions,
            PersistenceResourceLimit::Runs => Self::Runs,
            PersistenceResourceLimit::Context => Self::Context,
            PersistenceResourceLimit::Transcript => Self::Transcript,
            PersistenceResourceLimit::LogicalSequence => Self::LogicalSequence,
            PersistenceResourceLimit::CredentialGeneration => Self::CredentialGeneration,
            PersistenceResourceLimit::CredentialMutations => Self::CredentialMutations,
            PersistenceResourceLimit::ModelSelections => Self::ModelSelections,
            PersistenceResourceLimit::DataUsePolicies => Self::DataUsePolicies,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugNormalizationStage {
    Catalog,
    CallCount,
    DuplicateCallId,
    ArgumentJson,
    ToolFieldSet,
    ToolType,
    ReadWindow,
    EditBounds,
    Path,
    UnknownTool,
    ForbiddenChildTool,
    TaskBounds,
    FinalMessageMissing,
    FinalMessageEmpty,
    FinalMessageMultiple,
    ToolInFinal,
    ToolTurnMessage,
    OutputBytes,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugToolKind {
    Read,
    Write,
    Edit,
    Bash,
    WebSearch,
    Ipython,
    Legacy,
    Other,
}

impl From<ToolKind> for DebugToolKind {
    fn from(tool: ToolKind) -> Self {
        match tool {
            ToolKind::Read => Self::Read,
            ToolKind::Write => Self::Write,
            ToolKind::Edit => Self::Edit,
            ToolKind::Bash => Self::Bash,
            ToolKind::WebSearch => Self::WebSearch,
            ToolKind::Ipython => Self::Ipython,
            ToolKind::Task => Self::Other,
            ToolKind::ListDirectory
            | ToolKind::ReadFile
            | ToolKind::SearchText
            | ToolKind::EditFile
            | ToolKind::CreateFile
            | ToolKind::CreateDirectory
            | ToolKind::RunCommand => Self::Legacy,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugToolError {
    InvalidPath,
    NotFound,
    WrongNodeKind,
    LinkOrReparsePoint,
    ChangedDuringOperation,
    BinaryFile,
    InvalidUtf8,
    DigestMismatch,
    ReplacementNotFound,
    ReplacementAmbiguous,
    ReplacementOverlap,
    AlreadyExists,
    ResourceLimit,
    OutputLimit,
    TimedOut,
    InactivityTimeout,
    Cancelled,
    Interrupted,
    NotDispatched,
    Uncertain,
    WebSearchUncertain,
    Filesystem,
    Network,
    InvalidResponse,
    CredentialNotConfigured,
    DataUseRestricted,
    WebSearchUnavailable,
    KernelUnavailable,
    ExecutionFailed,
    ImageInputUnsupported,
    ModelUnavailable,
}
