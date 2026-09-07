use super::*;
use crate::persistence::{
    maintenance::{MaintenanceJobObservation, MaintenanceObservation},
    run_types::RecentProviderUsage,
};

impl Backend {
    pub(in crate::persistence::backend) fn maintenance_observation(
        &self,
        session: SessionId,
    ) -> Result<MaintenanceObservation, PersistenceError> {
        let row = self.connection.query_row(
            "SELECT job.state, run.open_code_service, run.model_id, job.source_entry_high_water,
                    json_extract(ready.result_payload, '$.input_tokens'), json_extract(ready.result_payload, '$.cached_input_tokens'),
                    json_extract(ready.result_payload, '$.cache_write_input_tokens'), json_extract(ready.result_payload, '$.output_tokens'),
                    ready.created_at_milliseconds - job.prepared_at_milliseconds
             FROM compaction_maintenance_jobs AS job JOIN run_accepted_facts AS run ON run.run_id = job.trigger_run_id
             LEFT JOIN compaction_maintenance_events AS ready ON ready.job_id = job.job_id AND ready.state = 3
             WHERE job.session_id = ?1 ORDER BY job.prepared_sequence DESC LIMIT 1", [&session.as_bytes()[..]],
            |row| Ok((row.get::<_, i64>(0)?, RunService::from_record(row.get(1)?)?, row.get::<_, String>(2)?,
                nonnegative_integer_from_row(row, 3)?, row.get::<_, Option<i64>>(4)?, row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<i64>>(6)?, row.get::<_, Option<i64>>(7)?, row.get::<_, Option<i64>>(8)?)),
        ).optional()?;
        let latest = row
            .map(
                |row| -> Result<MaintenanceJobObservation, PersistenceError> {
                    let count = |value: i64| u64::try_from(value).map_err(|_| invalid());
                    let usage = match (row.4, row.5, row.6, row.7, row.8) {
                        (Some(input), Some(cached), Some(writes), Some(output), Some(elapsed)) => {
                            Some(RecentProviderUsage {
                                input_tokens: count(input)?,
                                cached_input_tokens: count(cached)?,
                                cache_write_input_tokens: count(writes)?,
                                output_tokens: count(output)?,
                                elapsed_milliseconds: Some(count(elapsed)?),
                            })
                        }
                        (None, None, None, None, None) => None,
                        _ => return Err(invalid()),
                    };
                    Ok(MaintenanceJobObservation {
                        state: State::from_record(row.0)?,
                        service: row.1,
                        model_id: row.2,
                        source_entry_high_water: row.3,
                        usage,
                    })
                },
            )
            .transpose()?;
        Ok(MaintenanceObservation {
            enabled: self.maintenance_enabled,
            latest,
        })
    }
}
