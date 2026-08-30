mod creation;
mod queries;
mod records;
mod workspace_creation;

use rusqlite::Connection;

use super::{PersistenceError, database, paths::StoragePaths};

pub(crate) struct Backend {
    pub(super) connection: Connection,
    pub(super) paths: StoragePaths,
}

impl Backend {
    pub(crate) fn open(application_root: &std::path::Path) -> Result<Self, PersistenceError> {
        let paths = StoragePaths::prepare(application_root)?;
        let connection = database::open(&paths)?;
        let mut backend = Self { connection, paths };
        backend.recover_incomplete_session_creations()?;
        backend.validate_ready_workspaces()?;
        Ok(backend)
    }
}
