mod child_journal;
pub(super) mod command_execution;
mod compaction;
mod context_budget;
mod context_compaction;
pub(super) mod context_execution;
mod context_history;
mod context_status;
mod context_usage;
mod creation;
mod credential_mutation;
mod data_use;
mod default_model;
pub(super) mod image_attachment;
pub(super) mod local_command;
mod maintenance;
mod model_credentials;
pub(super) mod project_context;
mod queries;
mod records;
mod repository_import;
mod run_acceptance;
mod run_cancellation;
mod run_execution;
pub(super) mod run_queries;
mod run_records;
mod run_recovery;
mod server_stop;
mod session_delete;
mod session_events;
mod session_mutation;
mod settings;
mod task_binding;
mod tool_execution;
mod web_binding;
mod workspace_creation;
mod worktree_generation;

use rusqlite::Connection;

use crate::debug_log::{self, DebugStartupStage};

use super::{
    PersistenceError,
    credentials::{CredentialStore, openai::OpenAiCredentialStore},
    database,
    paths::StoragePaths,
};

pub(crate) struct Backend {
    pub(super) connection: Connection,
    pub(super) credentials: CredentialStore,
    pub(super) openai_credentials: OpenAiCredentialStore,
    pub(super) paths: StoragePaths,
    context_data_version: std::cell::Cell<Option<i64>>,
    maintenance_enabled: bool,
}

impl Backend {
    pub(crate) fn open(application_root: &std::path::Path) -> Result<Self, PersistenceError> {
        let paths = StoragePaths::prepare(application_root)?;
        let credentials = CredentialStore::open(application_root)?;
        let openai_credentials = OpenAiCredentialStore::open(application_root)?;
        let connection = database::open(&paths)?;
        let mut backend = Self {
            connection,
            credentials,
            openai_credentials,
            paths,
            context_data_version: std::cell::Cell::new(None),
            maintenance_enabled: false,
        };
        debug_log::startup_stage(DebugStartupStage::BackendRecovery, || {
            use DebugStartupStage::*;
            debug_log::startup_stage(AttachmentReconciliation, || {
                backend.reconcile_image_attachments()
            })?;
            debug_log::startup_stage(ContextIntegrity, || {
                backend.ensure_startup_context_integrity()
            })?;
            debug_log::startup_stage(CompactionRecovery, || {
                backend.recover_compaction_operations()
            })?;
            debug_log::startup_stage(CredentialMutationRecovery, || {
                backend.recover_credential_mutations()
            })?;
            debug_log::startup_stage(CredentialRefreshRecovery, || {
                backend.openai_credentials.recover_refresh()
            })?;
            debug_log::startup_stage(MaintenanceRecovery, || backend.recover_maintenance_jobs())?;
            debug_log::startup_stage(SessionCreationRecovery, || {
                backend.recover_incomplete_session_creations()
            })?;
            debug_log::startup_stage(ChildJournalRecovery, || backend.recover_child_journals())?;
            debug_log::startup_stage(ToolRecovery, || backend.recover_tool_operations())?;
            debug_log::startup_stage(LocalCommandRecovery, || backend.recover_local_commands())?;
            debug_log::startup_stage(RunRecovery, || backend.recover_nonterminal_runs())?;
            debug_log::startup_stage(SessionArchiveRecovery, || {
                backend.recover_session_archives()
            })?;
            debug_log::startup_stage(SessionDeleteRecovery, || backend.recover_session_deletes())
        })?;
        Ok(backend)
    }
}
