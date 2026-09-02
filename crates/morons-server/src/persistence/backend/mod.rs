pub(super) mod command_execution;
mod creation;
mod credential_mutation;
mod execution_image;
mod export;
mod queries;
mod records;
mod repository_import;
mod run_acceptance;
mod run_cancellation;
mod run_execution;
mod run_queries;
mod run_records;
mod run_recovery;
mod server_stop;
mod session_events;
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
        backend.recover_credential_mutations()?;
        backend.recover_execution_images()?;
        backend.recover_exports()?;
        backend.recover_incomplete_session_creations()?;
        backend.recover_repository_imports()?;
        backend.recover_worktree_layouts()?;
        backend.validate_ready_repositories()?;
        backend.validate_ready_workspaces()?;
        backend.recover_tool_operations()?;
        backend.recover_nonterminal_runs()?;
        Ok(backend)
    }
}
