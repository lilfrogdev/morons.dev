use rusqlite::{Connection, OptionalExtension as _, params};

use super::{
    Backend,
    records::{load_session, nonnegative_integer_from_row, sequence_to_sql},
};
use crate::persistence::{
    PersistenceError, RunId, SessionId,
    steering::{SteeringCursor, SteeringItem, SteeringNotice, SteeringPage, SteeringSnapshot},
};

const HISTORY: &str = "SELECT accepted_sequence AS sequence, queue_revision FROM steering_mutation_requests WHERE session_id = ?1
    UNION ALL SELECT fact_sequence, queue_revision FROM steering_lifecycle_facts WHERE session_id = ?1
    UNION ALL SELECT fact_sequence, queue_revision FROM steering_delivery_facts WHERE session_id = ?1";

fn high_water(
    connection: &Connection,
    session_id: SessionId,
) -> Result<SteeringCursor, PersistenceError> {
    if load_session(connection, session_id)?.is_none() {
        return Err(PersistenceError::SessionNotFound);
    }
    let sequence = connection.query_row(
        &format!("SELECT COALESCE(MAX(sequence), 0) FROM ({HISTORY})"),
        [session_id.as_bytes()],
        |row| nonnegative_integer_from_row(row, 0),
    )?;
    Ok(SteeringCursor {
        session_id,
        sequence,
    })
}

impl Backend {
    pub(crate) fn steering_snapshot(
        &mut self,
        session_id: SessionId,
    ) -> Result<SteeringSnapshot, PersistenceError> {
        let tx = self.connection.transaction()?;
        let cursor = high_water(&tx, session_id)?;
        let queue = tx
            .query_row(
                "SELECT revision, target_run_id, paused FROM steering_queues WHERE session_id = ?1",
                [session_id.as_bytes()],
                |row| {
                    Ok((
                        nonnegative_integer_from_row(row, 0)?,
                        RunId::from_bytes(row.get(1)?),
                        row.get::<_, bool>(2)?,
                    ))
                },
            )
            .optional()?;
        let mut statement = tx.prepare("SELECT item_id, revision, enqueue_sequence, text FROM steering_pending_messages WHERE session_id = ?1 ORDER BY enqueue_sequence LIMIT 16")?;
        let items = statement
            .query_map([session_id.as_bytes()], |row| {
                Ok(SteeringItem {
                    id: row.get(0)?,
                    revision: nonnegative_integer_from_row(row, 1)?,
                    enqueue_sequence: nonnegative_integer_from_row(row, 2)?,
                    text: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        tx.commit()?;
        Ok(SteeringSnapshot {
            cursor,
            revision: queue.map_or(0, |q| q.0),
            target_run_id: queue.map(|q| q.1),
            paused: queue.is_none_or(|q| q.2),
            items,
        })
    }

    pub(crate) fn steering_replay(
        &mut self,
        cursor: SteeringCursor,
        limit: u16,
    ) -> Result<SteeringPage, PersistenceError> {
        if !(1..=128).contains(&limit) {
            return Err(PersistenceError::InvalidInput {
                reason: "steering replay requires a limit between 1 and 128",
            });
        }
        let tx = self.connection.transaction()?;
        let high_water = high_water(&tx, cursor.session_id)?;
        if cursor.sequence > high_water.sequence {
            return Err(PersistenceError::InvalidInput {
                reason: "the steering cursor is ahead of durable history",
            });
        }
        let mut statement = tx.prepare(&format!("SELECT sequence, queue_revision FROM ({HISTORY}) WHERE sequence > ?2 AND sequence <= ?3 ORDER BY sequence LIMIT ?4"))?;
        let notices = statement
            .query_map(
                params![
                    cursor.session_id.as_bytes(),
                    sequence_to_sql(cursor.sequence)?,
                    sequence_to_sql(high_water.sequence)?,
                    limit
                ],
                |row| {
                    Ok(SteeringNotice {
                        cursor: SteeringCursor {
                            session_id: cursor.session_id,
                            sequence: nonnegative_integer_from_row(row, 0)?,
                        },
                        revision: nonnegative_integer_from_row(row, 1)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        tx.commit()?;
        Ok(SteeringPage {
            notices,
            high_water,
        })
    }
}
