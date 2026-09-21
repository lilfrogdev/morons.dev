use rusqlite::{Transaction, params};

use super::records::{next_sequence, sequence_to_sql, time_to_sql};
use crate::persistence::PersistenceError;

#[derive(Clone, Copy)]
pub(super) enum PauseReason {
    Cancellation = 1,
    Terminal = 2,
    Archive = 3,
    Recovery = 4,
}

pub(super) fn pause(
    transaction: &Transaction<'_>,
    session_id: Option<&[u8]>,
    run_id: Option<&[u8]>,
    reason: PauseReason,
    source_sequence: Option<u64>,
    now: u64,
) -> Result<(), PersistenceError> {
    let queues = {
        let mut statement = transaction.prepare(
            "SELECT session_id, target_run_id, revision FROM steering_queues
             WHERE paused = 0 AND (?1 IS NULL OR session_id = ?1)
               AND (?2 IS NULL OR target_run_id = ?2) ORDER BY session_id",
        )?;
        statement
            .query_map(params![session_id, run_id], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (session, target, revision) in queues {
        let revision = revision
            .checked_add(1)
            .ok_or(PersistenceError::InvalidState {
                reason: "steering queue revision overflow",
            })?;
        let sequence = next_sequence(transaction)?;
        transaction.execute(
            "INSERT INTO steering_lifecycle_facts
             (fact_sequence, session_id, target_run_id, queue_revision, reason,
              source_sequence, created_at_milliseconds)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                sequence_to_sql(sequence)?,
                session,
                target,
                revision,
                reason as i64,
                source_sequence.map(sequence_to_sql).transpose()?,
                time_to_sql(now)?
            ],
        )?;
        transaction.execute(
            "UPDATE steering_queues SET paused = 1, revision = ?2 WHERE session_id = ?1",
            params![session, revision],
        )?;
    }
    Ok(())
}
