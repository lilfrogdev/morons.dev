use rusqlite::{Connection, OptionalExtension as _, params};
use sha2::{Digest as _, Sha256};

use crate::{
    persistence::{PersistenceError, RunId},
    project_context::{MAX_SNAPSHOT_BYTES, RunProjectContext},
};

pub(super) fn insert(
    connection: &Connection,
    run_id: RunId,
    context: &RunProjectContext,
) -> Result<(), PersistenceError> {
    if !context.is_valid() {
        return Err(invalid());
    }
    let snapshot = serde_json::to_string(context).map_err(|_| invalid())?;
    connection.execute(
        "INSERT INTO run_project_contexts (run_id, snapshot, source_digest) VALUES (?1, ?2, ?3)",
        params![
            &run_id.as_bytes()[..],
            &snapshot,
            &digest(run_id, &snapshot)[..]
        ],
    )?;
    Ok(())
}

pub(crate) fn load(
    connection: &Connection,
    run_id: RunId,
) -> Result<Option<RunProjectContext>, PersistenceError> {
    let (version, snapshot, expected): (u16, Option<String>, Option<[u8; 32]>) = connection.query_row(
        "SELECT run.tool_catalog_version,
                CASE WHEN length(CAST(context.snapshot AS BLOB)) <= 65536 THEN context.snapshot ELSE NULL END,
                context.source_digest
         FROM run_accepted_facts AS run LEFT JOIN run_project_contexts AS context ON context.run_id = run.run_id
         WHERE run.run_id = ?1", [&run_id.as_bytes()[..]], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional()?.ok_or_else(invalid)?;
    if version < 9 {
        return if snapshot.is_none() && expected.is_none() {
            Ok(None)
        } else {
            Err(invalid())
        };
    }
    let snapshot = snapshot.ok_or_else(invalid)?;
    if version != 9
        || snapshot.len() > MAX_SNAPSHOT_BYTES
        || expected != Some(digest(run_id, &snapshot))
    {
        return Err(invalid());
    }
    let context: RunProjectContext = serde_json::from_str(&snapshot).map_err(|_| invalid())?;
    if !context.is_valid() {
        return Err(invalid());
    }
    Ok(Some(context))
}

fn digest(run_id: RunId, snapshot: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"morons.dev/run-project-context/v1\0");
    hash.update(run_id.as_bytes());
    hash.update(snapshot.as_bytes());
    hash.finalize().into()
}

fn invalid() -> PersistenceError {
    PersistenceError::InvalidState {
        reason: "a run project-context snapshot has invalid bounds, policy or integrity",
    }
}

#[cfg(test)]
mod tests;
