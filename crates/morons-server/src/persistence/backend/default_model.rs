use rusqlite::{OptionalExtension as _, TransactionBehavior, params};

use super::{
    Backend,
    records::{
        MUTATION_OPERATION_DEFAULT_MODEL, current_time_milliseconds, load_mutation_operation,
        next_sequence, sequence_to_sql, time_to_sql,
    },
};
use crate::persistence::{
    DefaultModelSelection, MutationRequestId, PersistenceError, PersistenceResourceLimit,
    RunOpenCodeService, types::REQUEST_FINGERPRINT_BYTES,
};

const MAX_DEFAULT_MODEL_SELECTIONS: i64 = 10_000;

impl Backend {
    pub(crate) fn default_model(&self) -> Result<Option<DefaultModelSelection>, PersistenceError> {
        self.connection
            .query_row(
                "SELECT open_code_service, model_id
                 FROM (
                    SELECT open_code_service, model_id, accepted_sequence AS sequence
                    FROM default_model_selections
                    UNION ALL
                    SELECT open_code_service, model_id, fact_sequence AS sequence
                    FROM run_accepted_facts
                 )
                 ORDER BY sequence DESC
                 LIMIT 1",
                [],
                |row| {
                    Ok(DefaultModelSelection {
                        service: RunOpenCodeService::from_record(row.get(0)?)?,
                        model_id: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(PersistenceError::from)
    }

    pub(crate) fn set_default_model(
        &mut self,
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        selection: DefaultModelSelection,
    ) -> Result<DefaultModelSelection, PersistenceError> {
        let operation = load_mutation_operation(&self.connection, request_id)?;
        let existing = self
            .connection
            .query_row(
                "SELECT operation_fingerprint, open_code_service, model_id
                 FROM default_model_selections WHERE request_id = ?1",
                [&request_id.as_bytes()[..]],
                |row| {
                    Ok((
                        row.get::<_, [u8; 32]>(0)?,
                        RunOpenCodeService::from_record(row.get(1)?)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        match (operation, existing) {
            (Some(MUTATION_OPERATION_DEFAULT_MODEL), Some(existing)) => {
                if existing.0 != fingerprint
                    || existing.1 != selection.service
                    || existing.2 != selection.model_id
                {
                    return Err(PersistenceError::RequestConflict);
                }
                return Ok(selection);
            }
            (Some(_), _) | (None, Some(_)) => return Err(PersistenceError::RequestConflict),
            (None, None) => {}
        }

        let selection_count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM default_model_selections",
            [],
            |row| row.get(0),
        )?;
        if selection_count >= MAX_DEFAULT_MODEL_SELECTIONS {
            return Err(PersistenceError::ResourceLimit {
                resource: PersistenceResourceLimit::ModelSelections,
            });
        }

        let now = current_time_milliseconds()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sequence = next_sequence(&transaction)?;
        transaction.execute(
            "INSERT INTO mutation_requests (
                request_id, operation_kind, accepted_sequence, accepted_at_milliseconds
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                &request_id.as_bytes()[..],
                MUTATION_OPERATION_DEFAULT_MODEL,
                sequence_to_sql(sequence)?,
                time_to_sql(now)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO default_model_selections (
                request_id, operation_fingerprint, open_code_service, model_id,
                accepted_sequence, accepted_at_milliseconds
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                &request_id.as_bytes()[..],
                &fingerprint[..],
                selection.service.to_record(),
                &selection.model_id,
                sequence_to_sql(sequence)?,
                time_to_sql(now)?,
            ],
        )?;
        transaction.commit()?;
        Ok(selection)
    }
}
