use morons_protocol::{
    ApplicationError, ApplicationResponse, ExecutionImageState as ProtocolExecutionImageState,
    ExecutionImageSummary as ProtocolExecutionImageSummary,
    ExecutionTargetArch as ProtocolExecutionTargetArch,
    ExecutionTargetOs as ProtocolExecutionTargetOs, MessageId as ProtocolMessageId,
    MutationRequestId as ProtocolMutationRequestId,
    OpenCodeCredentialStatus as ProtocolOpenCodeCredentialStatus, OpenCodeModelCapabilities,
    OpenCodeModelRetention, OpenCodeModelSummary, OpenCodeModelTrainingUse,
    OpenCodeService as ProtocolOpenCodeService, ResourceLimit,
    RunFailureKind as ProtocolRunFailureKind, RunId as ProtocolRunId, RunState as ProtocolRunState,
    RunSummary, SessionCatalogEventCursor as ProtocolSessionCatalogEventCursor,
    SessionEventCursor as ProtocolSessionEventCursor, SessionId as ProtocolSessionId,
    SessionListCursor as ProtocolSessionListCursor, SessionSummary,
    ToolCallId as ProtocolToolCallId, ToolKind as ProtocolToolKind,
    ToolResultStatus as ProtocolToolResultStatus, TranscriptCursor as ProtocolTranscriptCursor,
    TranscriptEntry as ProtocolTranscriptEntry,
    WorkspaceBlockReason as ProtocolWorkspaceBlockReason, WorkspaceState as ProtocolWorkspaceState,
    WorkspaceSummary as ProtocolWorkspaceSummary,
};

use super::ApplicationOutcome;
use crate::{
    persistence::{
        AcceptedRun, ExecutionImageState, ExecutionImageSummary, ExecutionTargetArch,
        ExecutionTargetOs, MutationRequestId, OpenCodeCredentialStatus, PersistenceError,
        PersistenceResourceLimit, Run, RunFailureKind, RunId, RunOpenCodeService, RunState,
        Session, SessionCatalogEventCursor, SessionEventCursor, SessionId, SessionListCursor,
        TranscriptCursor, TranscriptEntry, WorkspaceBlockReason, WorkspaceState, WorkspaceSummary,
    },
    provider::{ModelRetention, ModelTrainingUse, OpenCodeModelAvailability, OpenCodeService},
};

pub(super) fn input_accepted_response(accepted: AcceptedRun) -> ApplicationOutcome {
    ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted {
        user_message_id: ProtocolMessageId::from_bytes(*accepted.user_message_id.as_bytes()),
        run: to_run_summary(accepted.run),
    })
}

pub(super) const fn to_persistence_service(service: ProtocolOpenCodeService) -> RunOpenCodeService {
    match service {
        ProtocolOpenCodeService::Zen => RunOpenCodeService::Zen,
        ProtocolOpenCodeService::Go => RunOpenCodeService::Go,
    }
}

pub(super) const fn to_provider_service(service: ProtocolOpenCodeService) -> OpenCodeService {
    match service {
        ProtocolOpenCodeService::Zen => OpenCodeService::Zen,
        ProtocolOpenCodeService::Go => OpenCodeService::Go,
    }
}

const fn to_protocol_service(service: RunOpenCodeService) -> ProtocolOpenCodeService {
    match service {
        RunOpenCodeService::Zen => ProtocolOpenCodeService::Zen,
        RunOpenCodeService::Go => ProtocolOpenCodeService::Go,
    }
}

pub(super) fn to_persistence_mutation_id(
    request_id: ProtocolMutationRequestId,
) -> MutationRequestId {
    MutationRequestId::from_bytes(*request_id.as_bytes())
}

pub(super) fn to_persistence_session_id(session_id: ProtocolSessionId) -> SessionId {
    SessionId::from_bytes(*session_id.as_bytes())
}

pub(super) fn to_persistence_run_id(run_id: ProtocolRunId) -> RunId {
    RunId::from_bytes(*run_id.as_bytes())
}

pub(super) fn to_protocol_run_id(run_id: RunId) -> ProtocolRunId {
    ProtocolRunId::from_bytes(*run_id.as_bytes())
}

pub(super) fn to_persistence_list_cursor(cursor: ProtocolSessionListCursor) -> SessionListCursor {
    let bytes = cursor.as_bytes();
    let mut snapshot_event_sequence = [0_u8; 8];
    snapshot_event_sequence.copy_from_slice(&bytes[..8]);
    let mut after_created_sequence = [0_u8; 8];
    after_created_sequence.copy_from_slice(&bytes[8..]);
    SessionListCursor::new(
        u64::from_be_bytes(snapshot_event_sequence),
        u64::from_be_bytes(after_created_sequence),
    )
}

pub(super) fn to_persistence_transcript_cursor(
    cursor: ProtocolTranscriptCursor,
) -> TranscriptCursor {
    let bytes = cursor.as_bytes();
    let mut session_id = [0_u8; 16];
    session_id.copy_from_slice(&bytes[..16]);
    let mut snapshot_entry_sequence = [0_u8; 8];
    snapshot_entry_sequence.copy_from_slice(&bytes[16..24]);
    let mut snapshot_event_sequence = [0_u8; 8];
    snapshot_event_sequence.copy_from_slice(&bytes[24..32]);
    let mut after_entry_sequence = [0_u8; 8];
    after_entry_sequence.copy_from_slice(&bytes[32..]);
    TranscriptCursor::new(
        SessionId::from_bytes(session_id),
        u64::from_be_bytes(snapshot_entry_sequence),
        u64::from_be_bytes(snapshot_event_sequence),
        u64::from_be_bytes(after_entry_sequence),
    )
}

pub(super) fn to_protocol_transcript_cursor(cursor: TranscriptCursor) -> ProtocolTranscriptCursor {
    let mut bytes = [0_u8; 40];
    bytes[..16].copy_from_slice(cursor.session_id().as_bytes());
    bytes[16..24].copy_from_slice(&cursor.snapshot_entry_sequence().to_be_bytes());
    bytes[24..32].copy_from_slice(&cursor.snapshot_event_sequence().to_be_bytes());
    bytes[32..].copy_from_slice(&cursor.after_entry_sequence().to_be_bytes());
    ProtocolTranscriptCursor::from_bytes(bytes)
}

pub(super) fn to_protocol_list_cursor(cursor: SessionListCursor) -> ProtocolSessionListCursor {
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&cursor.snapshot_event_sequence().to_be_bytes());
    bytes[8..].copy_from_slice(&cursor.after_created_sequence().to_be_bytes());
    ProtocolSessionListCursor::from_bytes(bytes)
}

pub(super) fn to_persistence_catalog_cursor(
    cursor: ProtocolSessionCatalogEventCursor,
) -> SessionCatalogEventCursor {
    SessionCatalogEventCursor::from_sequence(u64::from_be_bytes(*cursor.as_bytes()))
}

pub(super) fn to_protocol_catalog_cursor(
    cursor: SessionCatalogEventCursor,
) -> ProtocolSessionCatalogEventCursor {
    ProtocolSessionCatalogEventCursor::from_bytes(cursor.sequence().to_be_bytes())
}

pub(super) fn to_persistence_session_event_cursor(
    cursor: ProtocolSessionEventCursor,
) -> SessionEventCursor {
    let bytes = cursor.as_bytes();
    let mut session_id = [0_u8; 16];
    session_id.copy_from_slice(&bytes[..16]);
    let mut sequence = [0_u8; 8];
    sequence.copy_from_slice(&bytes[16..]);
    SessionEventCursor::new(
        SessionId::from_bytes(session_id),
        u64::from_be_bytes(sequence),
    )
}

pub(super) fn to_protocol_session_event_cursor(
    cursor: SessionEventCursor,
) -> ProtocolSessionEventCursor {
    let mut bytes = [0_u8; 24];
    bytes[..16].copy_from_slice(cursor.session_id().as_bytes());
    bytes[16..].copy_from_slice(&cursor.sequence().to_be_bytes());
    ProtocolSessionEventCursor::from_bytes(bytes)
}

pub(super) fn to_protocol_model_summary(
    availability: OpenCodeModelAvailability,
) -> Option<OpenCodeModelSummary> {
    let model = availability.model;
    let training_use = match model.data_use.training {
        ModelTrainingUse::NotUsed => OpenCodeModelTrainingUse::NotUsed,
        ModelTrainingUse::MayUsePromptsAndCompletions => return None,
    };
    let retention = match model.data_use.retention {
        ModelRetention::None => OpenCodeModelRetention::None,
        ModelRetention::UpToThirtyDays => OpenCodeModelRetention::UpToThirtyDays,
    };
    Some(OpenCodeModelSummary {
        service: match model.service {
            OpenCodeService::Zen => ProtocolOpenCodeService::Zen,
            OpenCodeService::Go => ProtocolOpenCodeService::Go,
        },
        id: model.id.to_owned(),
        display_name: model.display_name.to_owned(),
        available: availability.available,
        responses_protocol_revision: model.responses_protocol_revision,
        capabilities: OpenCodeModelCapabilities {
            text_input: model.capabilities.text_input,
            text_output: model.capabilities.text_output,
            reasoning: model.capabilities.reasoning,
            reasoning_continuation: model.capabilities.reasoning_continuation,
            tool_calls: model.capabilities.tool_calls,
        },
        maximum_input_tokens: model.maximum_input_tokens,
        maximum_output_tokens: model.maximum_output_tokens,
        training_use,
        retention,
    })
}

pub(super) const fn to_protocol_credential_status(
    credential: OpenCodeCredentialStatus,
) -> ProtocolOpenCodeCredentialStatus {
    ProtocolOpenCodeCredentialStatus {
        configured: credential.configured,
        generation: credential.generation,
    }
}

pub(super) const fn to_protocol_execution_image_summary(
    image: ExecutionImageSummary,
) -> ProtocolExecutionImageSummary {
    ProtocolExecutionImageSummary {
        state: match image.state {
            ExecutionImageState::Unconfigured => ProtocolExecutionImageState::Unconfigured,
            ExecutionImageState::Provisioning => ProtocolExecutionImageState::Provisioning,
            ExecutionImageState::Ready => ProtocolExecutionImageState::Ready,
            ExecutionImageState::Blocked => ProtocolExecutionImageState::Blocked,
        },
        target_os: match image.target_os {
            ExecutionTargetOs::Macos => ProtocolExecutionTargetOs::Macos,
            ExecutionTargetOs::Linux => ProtocolExecutionTargetOs::Linux,
            ExecutionTargetOs::Windows => ProtocolExecutionTargetOs::Windows,
        },
        target_arch: match image.target_arch {
            ExecutionTargetArch::X86_64 => ProtocolExecutionTargetArch::X86_64,
            ExecutionTargetArch::Aarch64 => ProtocolExecutionTargetArch::Aarch64,
        },
        format_version: image.format_version,
        limits_version: image.limits_version,
        file_count: image.file_count,
        logical_bytes: image.logical_bytes,
    }
}

pub(super) fn to_protocol_workspace_summary(
    workspace: WorkspaceSummary,
) -> ProtocolWorkspaceSummary {
    ProtocolWorkspaceSummary {
        state: match workspace.state {
            WorkspaceState::Empty => ProtocolWorkspaceState::Empty,
            WorkspaceState::Importing => ProtocolWorkspaceState::Importing,
            WorkspaceState::Ready => ProtocolWorkspaceState::Ready,
            WorkspaceState::Blocked => ProtocolWorkspaceState::Blocked,
        },
        file_count: workspace.file_count,
        logical_bytes: workspace.logical_bytes,
        block_reason: match workspace.block_reason {
            Some(WorkspaceBlockReason::InconsistentImportState) => {
                Some(ProtocolWorkspaceBlockReason::InconsistentImportState)
            }
            Some(WorkspaceBlockReason::UncertainToolEffect) => {
                Some(ProtocolWorkspaceBlockReason::UncertainToolEffect)
            }
            None => None,
        },
        blocked_run_id: workspace.blocked_run_id.map(to_protocol_run_id),
        blocked_tool: workspace.blocked_tool.map(to_protocol_tool_kind),
    }
}

pub(super) fn to_run_summary(run: Run) -> RunSummary {
    RunSummary {
        id: to_protocol_run_id(run.id),
        session_id: ProtocolSessionId::from_bytes(*run.session_id.as_bytes()),
        user_message_id: ProtocolMessageId::from_bytes(*run.user_message_id.as_bytes()),
        service: to_protocol_service(run.service),
        model_id: run.model_id,
        protocol_revision: run.protocol_revision,
        credential_generation: run.credential_generation,
        context_policy_version: run.context_policy_version,
        tool_catalog_version: run.tool_catalog_version,
        tool_limits_version: run.tool_limits_version,
        state: to_protocol_run_state(run.state),
        cancellation_requested: run.cancellation_requested,
        failure: run.failure.map(to_protocol_run_failure),
        accepted_at_milliseconds: run.accepted_at_milliseconds,
        updated_at_milliseconds: run.updated_at_milliseconds,
    }
}

pub(super) const fn to_protocol_run_state(state: RunState) -> ProtocolRunState {
    match state {
        RunState::Accepted => ProtocolRunState::Accepted,
        RunState::Active => ProtocolRunState::Active,
        RunState::Succeeded => ProtocolRunState::Succeeded,
        RunState::Failed => ProtocolRunState::Failed,
        RunState::Cancelled => ProtocolRunState::Cancelled,
        RunState::Interrupted => ProtocolRunState::Interrupted,
        RunState::Uncertain => ProtocolRunState::Uncertain,
    }
}

const fn to_protocol_run_failure(failure: RunFailureKind) -> ProtocolRunFailureKind {
    match failure {
        RunFailureKind::CredentialChanged => ProtocolRunFailureKind::CredentialChanged,
        RunFailureKind::CredentialNotConfigured => ProtocolRunFailureKind::CredentialNotConfigured,
        RunFailureKind::AuthenticationOrEntitlement => {
            ProtocolRunFailureKind::AuthenticationOrEntitlement
        }
        RunFailureKind::RateLimited => ProtocolRunFailureKind::RateLimited,
        RunFailureKind::ProviderUnavailable => ProtocolRunFailureKind::ProviderUnavailable,
        RunFailureKind::ProviderRejected => ProtocolRunFailureKind::ProviderRejected,
        RunFailureKind::ProviderProtocol => ProtocolRunFailureKind::ProviderProtocol,
        RunFailureKind::InvalidProviderOutput => ProtocolRunFailureKind::InvalidProviderOutput,
        RunFailureKind::ToolExecution => ProtocolRunFailureKind::ToolExecution,
        RunFailureKind::ResourceLimit => ProtocolRunFailureKind::ResourceLimit,
        RunFailureKind::Internal => ProtocolRunFailureKind::Internal,
    }
}

pub(super) fn to_protocol_transcript_entry(entry: TranscriptEntry) -> ProtocolTranscriptEntry {
    match entry {
        TranscriptEntry::UserMessage {
            id,
            run_id,
            text,
            created_at_milliseconds,
            ..
        } => ProtocolTranscriptEntry::UserMessage {
            id: ProtocolMessageId::from_bytes(*id.as_bytes()),
            run_id: to_protocol_run_id(run_id),
            text,
            created_at_milliseconds,
        },
        TranscriptEntry::AssistantMessage {
            id,
            run_id,
            service,
            model_id,
            text,
            refusal,
            created_at_milliseconds,
            ..
        } => ProtocolTranscriptEntry::AssistantMessage {
            id: ProtocolMessageId::from_bytes(*id.as_bytes()),
            run_id: to_protocol_run_id(run_id),
            service: to_protocol_service(service),
            model_id,
            text,
            refusal,
            created_at_milliseconds,
        },
        TranscriptEntry::ToolCall {
            id,
            run_id,
            call_id,
            input,
            created_at_milliseconds,
            ..
        } => ProtocolTranscriptEntry::ToolCall {
            id: ProtocolMessageId::from_bytes(*id.as_bytes()),
            run_id: to_protocol_run_id(run_id),
            call_id: ProtocolToolCallId::from_bytes(*call_id.as_bytes()),
            tool: to_protocol_tool_kind(input.kind()),
            path: input.path().as_str().to_owned(),
            created_at_milliseconds,
        },
        TranscriptEntry::ToolResult {
            id,
            run_id,
            call_id,
            tool,
            result,
            created_at_milliseconds,
            ..
        } => ProtocolTranscriptEntry::ToolResult {
            id: ProtocolMessageId::from_bytes(*id.as_bytes()),
            run_id: to_protocol_run_id(run_id),
            call_id: ProtocolToolCallId::from_bytes(*call_id.as_bytes()),
            tool: to_protocol_tool_kind(tool),
            status: protocol_tool_result_status(&result),
            summary: result.summary(),
            created_at_milliseconds,
        },
    }
}

const fn to_protocol_tool_kind(tool: crate::tools::ToolKind) -> ProtocolToolKind {
    match tool {
        crate::tools::ToolKind::ListDirectory => ProtocolToolKind::ListDirectory,
        crate::tools::ToolKind::ReadFile => ProtocolToolKind::ReadFile,
        crate::tools::ToolKind::SearchText => ProtocolToolKind::SearchText,
        crate::tools::ToolKind::EditFile => ProtocolToolKind::EditFile,
        crate::tools::ToolKind::CreateFile => ProtocolToolKind::CreateFile,
        crate::tools::ToolKind::CreateDirectory => ProtocolToolKind::CreateDirectory,
        crate::tools::ToolKind::RunCommand => ProtocolToolKind::RunCommand,
    }
}

const fn protocol_tool_result_status(
    result: &crate::tools::ToolResult,
) -> ProtocolToolResultStatus {
    match result {
        crate::tools::ToolResult::Ok { .. } => ProtocolToolResultStatus::Succeeded,
        crate::tools::ToolResult::Error {
            error: crate::tools::ToolErrorKind::Uncertain,
        } => ProtocolToolResultStatus::Uncertain,
        crate::tools::ToolResult::Error {
            error:
                crate::tools::ToolErrorKind::Interrupted
                | crate::tools::ToolErrorKind::Cancelled
                | crate::tools::ToolErrorKind::NotDispatched,
        } => ProtocolToolResultStatus::Interrupted,
        crate::tools::ToolResult::Error { .. } => ProtocolToolResultStatus::Failed,
    }
}

pub(super) fn to_session_summary(session: Session) -> SessionSummary {
    SessionSummary {
        id: ProtocolSessionId::from_bytes(*session.id.as_bytes()),
        display_name: session.display_name,
        created_at_milliseconds: session.created_at_milliseconds,
    }
}

pub(super) fn to_application_error(error: PersistenceError) -> ApplicationError {
    if matches!(
        &error,
        PersistenceError::Io(_)
            | PersistenceError::Sqlite(_)
            | PersistenceError::Control(_)
            | PersistenceError::Randomness(_)
            | PersistenceError::InvalidState { .. }
            | PersistenceError::WorkerStopped
    ) {
        eprintln!("session application operation failed: {error}");
    }

    match error {
        PersistenceError::InvalidInput { .. } => ApplicationError::InvalidRequest,
        PersistenceError::RequestConflict => ApplicationError::RequestConflict,
        PersistenceError::SessionNotFound => ApplicationError::SessionNotFound,
        PersistenceError::RunNotFound => ApplicationError::RunNotFound,
        PersistenceError::SessionBusy { active_run_id } => ApplicationError::SessionBusy {
            active_run_id: to_protocol_run_id(active_run_id),
        },
        PersistenceError::CredentialGenerationConflict => {
            ApplicationError::CredentialGenerationConflict
        }
        PersistenceError::CredentialNotConfigured => {
            ApplicationError::OpenCodeCredentialNotConfigured
        }
        PersistenceError::CredentialMutationNotApplied => {
            ApplicationError::CredentialMutationNotApplied
        }
        PersistenceError::ExecutionImageProvisionNotApplied => {
            ApplicationError::ExecutionImageProvisionNotApplied
        }
        PersistenceError::ExecutionImageBlocked => ApplicationError::ExecutionImageBlocked,
        PersistenceError::ReviewCursorStale => ApplicationError::ReviewCursorStale,
        PersistenceError::ReviewUnavailable => ApplicationError::ReviewUnavailable,
        PersistenceError::WorkspaceNotPristine => ApplicationError::WorkspaceNotPristine,
        PersistenceError::WorkspaceBusy => ApplicationError::WorkspaceBusy,
        PersistenceError::RepositoryAlreadyImported => ApplicationError::RepositoryAlreadyImported,
        PersistenceError::RepositoryImportNotApplied => {
            ApplicationError::RepositoryImportNotApplied
        }
        PersistenceError::WorkspaceBlocked | PersistenceError::ToolUncertaintyNotFound => {
            ApplicationError::WorkspaceBlocked
        }
        PersistenceError::ResourceLimit {
            resource: PersistenceResourceLimit::Sessions,
        } => ApplicationError::ResourceLimit {
            resource: ResourceLimit::Sessions,
        },
        PersistenceError::ResourceLimit {
            resource: PersistenceResourceLimit::Runs,
        } => ApplicationError::ResourceLimit {
            resource: ResourceLimit::Runs,
        },
        PersistenceError::ResourceLimit {
            resource: PersistenceResourceLimit::Context,
        } => ApplicationError::ResourceLimit {
            resource: ResourceLimit::Context,
        },
        PersistenceError::ResourceLimit {
            resource:
                PersistenceResourceLimit::Transcript
                | PersistenceResourceLimit::LogicalSequence
                | PersistenceResourceLimit::CredentialGeneration
                | PersistenceResourceLimit::CredentialMutations
                | PersistenceResourceLimit::Workspace
                | PersistenceResourceLimit::ExecutionImage,
        } => ApplicationError::ResourceLimit {
            resource: ResourceLimit::Storage,
        },
        PersistenceError::WorkerStopped => ApplicationError::ServiceUnavailable,
        PersistenceError::Io(_)
        | PersistenceError::Sqlite(_)
        | PersistenceError::Control(_)
        | PersistenceError::Randomness(_)
        | PersistenceError::InvalidState { .. } => ApplicationError::Internal,
    }
}
