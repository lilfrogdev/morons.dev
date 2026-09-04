pub(super) mod command_execution;
mod compaction;
mod creation;
mod credential_mutation;
mod default_model;
pub(super) mod image_attachment;
pub(super) mod local_command;
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
mod tool_execution;
mod workspace_creation;
mod worktree_generation;

use rusqlite::Connection;

use super::{PersistenceError, credentials::CredentialStore, database, paths::StoragePaths};

pub(crate) struct Backend {
    pub(super) connection: Connection,
    pub(super) credentials: CredentialStore,
    pub(super) paths: StoragePaths,
}

impl Backend {
    pub(crate) fn open(application_root: &std::path::Path) -> Result<Self, PersistenceError> {
        let paths = StoragePaths::prepare(application_root)?;
        let credentials = CredentialStore::open(application_root)?;
        let connection = database::open(&paths)?;
        let mut backend = Self {
            connection,
            credentials,
            paths,
        };
        backend.reconcile_image_attachments()?;
        backend.validate_context_checkpoint_digests()?;
        backend.recover_compaction_operations()?;
        backend.recover_credential_mutations()?;
        backend.recover_incomplete_session_creations()?;
        backend.recover_tool_operations()?;
        backend.recover_local_commands()?;
        backend.recover_nonterminal_runs()?;
        backend.recover_session_archives()?;
        backend.recover_session_deletes()?;
        Ok(backend)
    }
}
