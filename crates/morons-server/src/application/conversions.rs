use morons_protocol::{
    ApplicationError, ApplicationResponse, MessageId as ProtocolMessageId,
    MutationRequestId as ProtocolMutationRequestId,
    OpenCodeCredentialStatus as ProtocolOpenCodeCredentialStatus,
    OpenCodeService as ProtocolOpenCodeService, ResourceLimit,
    RunFailureKind as ProtocolRunFailureKind, RunId as ProtocolRunId, RunState as ProtocolRunState,
    RunSummary, SessionCatalogEventCursor as ProtocolSessionCatalogEventCursor,
    SessionId as ProtocolSessionId, SessionListCursor as ProtocolSessionListCursor, SessionSummary,
    TranscriptCursor as ProtocolTranscriptCursor, TranscriptEntry as ProtocolTranscriptEntry,
};

use super::ApplicationOutcome;
use crate::{
    persistence::{
        AcceptedRun, MutationRequestId, OpenCodeCredentialStatus, PersistenceError,
        PersistenceResourceLimit, Run, RunFailureKind, RunId, RunOpenCodeService, RunState,
        Session, SessionCatalogEventCursor, SessionId, SessionListCursor, TranscriptCursor,
        TranscriptEntry,
    },
    provider::OpenCodeService,
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
    let mut after_entry_sequence = [0_u8; 8];
    after_entry_sequence.copy_from_slice(&bytes[24..]);
    TranscriptCursor::new(
        SessionId::from_bytes(session_id),
        u64::from_be_bytes(snapshot_entry_sequence),
        u64::from_be_bytes(after_entry_sequence),
    )
}

pub(super) fn to_protocol_transcript_cursor(cursor: TranscriptCursor) -> ProtocolTranscriptCursor {
    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(cursor.session_id().as_bytes());
    bytes[16..24].copy_from_slice(&cursor.snapshot_entry_sequence().to_be_bytes());
    bytes[24..].copy_from_slice(&cursor.after_entry_sequence().to_be_bytes());
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

pub(super) const fn to_protocol_credential_status(
    credential: OpenCodeCredentialStatus,
) -> ProtocolOpenCodeCredentialStatus {
    ProtocolOpenCodeCredentialStatus {
        configured: credential.configured,
        generation: credential.generation,
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
                | PersistenceResourceLimit::CredentialMutations,
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
