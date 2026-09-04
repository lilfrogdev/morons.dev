use rusqlite::{OptionalExtension as _, TransactionBehavior, params};

use super::{
    Backend,
    records::{
        MUTATION_OPERATION_SUBAGENT_MODEL, current_time_milliseconds, load_mutation_operation,
        next_sequence, sequence_to_sql, time_to_sql,
    },
};
use crate::persistence::{
    MutationRequestId, PersistenceError, PersistenceResourceLimit, RunOpenCodeService,
    SubagentModelSetting, types::REQUEST_FINGERPRINT_BYTES,
};

const MAX_SUBAGENT_MODEL_SELECTIONS: i64 = 10_000;

impl Backend {
    pub(crate) fn subagent_model_setting(&self) -> Result<SubagentModelSetting, PersistenceError> {
        self.connection
            .query_row(
                "SELECT selection_kind, open_code_service, model_id
                 FROM subagent_model_selections
                 ORDER BY accepted_sequence DESC
                 LIMIT 1",
                [],
                subagent_model_from_row,
            )
            .optional()
            .map(|setting| setting.unwrap_or(SubagentModelSetting::InheritParent {}))
            .map_err(PersistenceError::from)
    }

    pub(crate) fn set_subagent_model_setting(
        &mut self,
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        setting: SubagentModelSetting,
    ) -> Result<SubagentModelSetting, PersistenceError> {
        let operation = load_mutation_operation(&self.connection, request_id)?;
        let existing = self
            .connection
            .query_row(
                "SELECT operation_fingerprint, selection_kind, open_code_service, model_id
                 FROM subagent_model_selections WHERE request_id = ?1",
                [&request_id.as_bytes()[..]],
                |row| {
                    Ok((
                        row.get::<_, [u8; 32]>(0)?,
                        subagent_model_from_columns(row, 1, 2, 3)?,
                    ))
                },
            )
            .optional()?;
        match (operation, existing) {
            (Some(MUTATION_OPERATION_SUBAGENT_MODEL), Some(existing)) => {
                if existing.0 != fingerprint || existing.1 != setting {
                    return Err(PersistenceError::RequestConflict);
                }
                return Ok(setting);
            }
            (Some(_), _) | (None, Some(_)) => return Err(PersistenceError::RequestConflict),
            (None, None) => {}
        }

        let selection_count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM subagent_model_selections",
            [],
            |row| row.get(0),
        )?;
        if selection_count >= MAX_SUBAGENT_MODEL_SELECTIONS {
            return Err(PersistenceError::ResourceLimit {
                resource: PersistenceResourceLimit::ModelSelections,
            });
        }

        let (selection_kind, service, model_id) = setting_to_record(&setting);
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
                MUTATION_OPERATION_SUBAGENT_MODEL,
                sequence_to_sql(sequence)?,
                time_to_sql(now)?,
            ],
        )?;
        transaction.execute(
            "INSERT INTO subagent_model_selections (
                request_id, operation_fingerprint, selection_kind, open_code_service, model_id,
                accepted_sequence, accepted_at_milliseconds
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                &request_id.as_bytes()[..],
                &fingerprint[..],
                selection_kind,
                service,
                model_id,
                sequence_to_sql(sequence)?,
                time_to_sql(now)?,
            ],
        )?;
        transaction.commit()?;
        Ok(setting)
    }
}

fn subagent_model_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SubagentModelSetting> {
    subagent_model_from_columns(row, 0, 1, 2)
}

fn subagent_model_from_columns(
    row: &rusqlite::Row<'_>,
    kind_index: usize,
    service_index: usize,
    model_index: usize,
) -> rusqlite::Result<SubagentModelSetting> {
    let kind = row.get::<_, i64>(kind_index)?;
    let service = row.get::<_, Option<i64>>(service_index)?;
    let model_id = row.get::<_, Option<String>>(model_index)?;
    match (kind, service, model_id) {
        (1, None, None) => Ok(SubagentModelSetting::InheritParent {}),
        (2, Some(service), Some(model_id)) => Ok(SubagentModelSetting::OpenCode {
            service: RunOpenCodeService::from_record(service)?,
            model_id,
        }),
        _ => Err(rusqlite::Error::InvalidColumnType(
            kind_index,
            "selection_kind".to_owned(),
            rusqlite::types::Type::Integer,
        )),
    }
}

fn setting_to_record(setting: &SubagentModelSetting) -> (i64, Option<i64>, Option<&str>) {
    match setting {
        SubagentModelSetting::InheritParent {} => (1, None, None),
        SubagentModelSetting::OpenCode { service, model_id } => {
            (2, Some(service.to_record()), Some(model_id))
        }
    }
}
