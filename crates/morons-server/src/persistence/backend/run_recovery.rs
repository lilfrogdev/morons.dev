use rusqlite::OptionalExtension;

use super::{Backend, run_records::PROVIDER_FACT_PREPARED};
use crate::persistence::{PersistenceError, RunId, run_types::ProviderOperationId};

impl Backend {
    pub(super) fn recover_nonterminal_runs(&mut self) -> Result<(), PersistenceError> {
        let runs = {
            let mut statement = self.connection.prepare(
                "SELECT run_id
                 FROM runs
                 WHERE state IN (1, 2)
                 ORDER BY accepted_sequence",
            )?;
            statement
                .query_map([], |row| row.get::<_, [u8; 16]>(0).map(RunId::from_bytes))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for run_id in runs {
            let operation_id = self
                .connection
                .query_row(
                    "SELECT operation_id
                     FROM provider_operation_facts AS prepared
                     WHERE run_id = ?1 AND fact_kind = ?2
                       AND NOT EXISTS (
                           SELECT 1 FROM provider_operation_facts AS terminal
                           WHERE terminal.operation_id = prepared.operation_id
                             AND terminal.fact_kind IN (3, 4, 5, 6)
                       )",
                    rusqlite::params![&run_id.as_bytes()[..], PROVIDER_FACT_PREPARED],
                    |row| {
                        row.get::<_, [u8; 16]>(0)
                            .map(ProviderOperationId::from_bytes)
                    },
                )
                .optional()?;
            self.finish_run_stopped(run_id, operation_id)?;
        }
        Ok(())
    }
}
