use rusqlite::OptionalExtension;

use super::Backend;
use crate::persistence::PersistenceError;

impl Backend {
    pub(crate) fn active_worktree_generation(
        &self,
        workspace_id: &[u8; 16],
    ) -> Result<[u8; 16], PersistenceError> {
        self.connection
            .query_row(
                "SELECT generation_id FROM active_worktree_generations WHERE workspace_id = ?1",
                [&workspace_id[..]],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(PersistenceError::WorkspaceBlocked)
    }
}
