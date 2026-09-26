use rusqlite::{OptionalExtension as _, TransactionBehavior, params};

use super::{
    Backend,
    records::{
        current_time_milliseconds, load_mutation_operation, next_sequence, random_identifier,
        sequence_to_sql, time_to_sql,
    },
};
use crate::persistence::{
    PersistenceError,
    steering::{SteeringChange, SteeringMutation, SteeringReceipt, validate_text},
};

impl Backend {
    pub(crate) fn lookup_steering_mutation(
        &self,
        mutation: &SteeringMutation,
    ) -> Result<Option<SteeringReceipt>, PersistenceError> {
        if mutation.request_id.is_zero() {
            return Err(PersistenceError::InvalidInput {
                reason: "a steering mutation requires a nonzero request identifier",
            });
        }
        let fingerprint = crate::persistence::steering::fingerprint(
            mutation.session_id,
            mutation.expected_revision,
            &mutation.change,
        );
        let existing = self.connection.query_row(
            "SELECT operation_fingerprint, accepted_sequence, queue_revision, item_id, item_revision
             FROM steering_mutation_requests WHERE request_id = ?1",
            [mutation.request_id.as_bytes()], |row| Ok((row.get::<_, [u8; 32]>(0)?, SteeringReceipt {
                sequence: read_unsigned(row, 1)?, queue_revision: read_unsigned(row, 2)?, item_id: row.get(3)?, item_revision: row.get::<_, Option<i64>>(4)?.map(|value| unsigned(value, 4)).transpose()?,
            }))).optional()?;
        match (
            load_mutation_operation(&self.connection, mutation.request_id)?,
            existing,
        ) {
            (Some(18), Some((stored, result))) if stored == fingerprint => Ok(Some(result)),
            (None, None) => Ok(None),
            _ => Err(PersistenceError::RequestConflict),
        }
    }

    pub(crate) fn mutate_steering(
        &mut self,
        mutation: SteeringMutation,
    ) -> Result<SteeringReceipt, PersistenceError> {
        use SteeringChange::{Edit, Enqueue, Pause, Remove, Resume};
        if let Some(receipt) = self.lookup_steering_mutation(&mutation)? {
            return Ok(receipt);
        }
        let fingerprint = crate::persistence::steering::fingerprint(
            mutation.session_id,
            mutation.expected_revision,
            &mutation.change,
        );
        if let Enqueue { text, .. } | Edit { text, .. } = &mutation.change {
            validate_text(text)?;
        }
        let session = self
            .get_session(mutation.session_id)?
            .ok_or(PersistenceError::SessionNotFound)?;
        if session.archived {
            return Err(PersistenceError::SessionArchived);
        }
        let now = current_time_milliseconds()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session_bytes = mutation.session_id.as_bytes();
        let archive_pending: bool = tx.query_row(
            "SELECT COALESCE((SELECT archived FROM session_archive_requests
             WHERE session_id = ?1 ORDER BY accepted_sequence DESC LIMIT 1), 0) = 1",
            [session_bytes],
            |row| row.get(0),
        )?;
        if archive_pending {
            return Err(PersistenceError::SessionArchived);
        }
        let queue = tx
            .query_row(
                "SELECT revision, target_run_id FROM steering_queues WHERE session_id = ?1",
                [session_bytes],
                |r| Ok((read_unsigned(r, 0)?, r.get::<_, [u8; 16]>(1)?)),
            )
            .optional()?;
        if queue.map_or(0, |q| q.0) != mutation.expected_revision {
            return Err(PersistenceError::RequestConflict);
        }
        let revision = mutation
            .expected_revision
            .checked_add(1)
            .ok_or(PersistenceError::RequestConflict)?;
        sequence_to_sql(revision)?;
        if let Enqueue { run_id, .. } | Resume { run_id } = &mutation.change {
            if !super::steering_queries::target_accepts_steering(&tx, mutation.session_id, *run_id)?
            {
                return Err(PersistenceError::RequestConflict);
            }
            if matches!(mutation.change, Enqueue { .. })
                && queue.is_some_and(|q| q.1 != *run_id.as_bytes())
            {
                return Err(PersistenceError::RequestConflict);
            }
        }
        let sequence = next_sequence(&tx)?;
        let mut receipt = SteeringReceipt {
            sequence,
            queue_revision: revision,
            item_id: None,
            item_revision: None,
        };
        match &mutation.change {
            Enqueue { run_id, text } => {
                tx.execute(
                    "INSERT INTO steering_queues (session_id, target_run_id, revision, paused)
                    VALUES (?1, ?2, 1, 1) ON CONFLICT (session_id) DO NOTHING",
                    params![session_bytes, run_id.as_bytes()],
                )?;
                let slot: Option<i64> = tx.query_row(
                    "WITH RECURSIVE slots(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM slots WHERE n<16)
                     SELECT MIN(n) FROM slots WHERE n NOT IN (SELECT slot FROM steering_pending_messages WHERE session_id = ?1)",
                    [session_bytes], |r| r.get(0))?;
                let slot = slot.ok_or(PersistenceError::InvalidInput {
                    reason: "the steering queue is full",
                })?;
                let item_id = random_identifier()?;
                tx.execute("INSERT INTO steering_pending_messages
                    (item_id, session_id, slot, enqueue_sequence, revision, text, actor, created_at_milliseconds)
                    VALUES (?1, ?2, ?3, ?4, 1, ?5, 1, ?6)",
                    params![item_id, session_bytes, slot, sequence_to_sql(sequence)?, text, time_to_sql(now)?])?;
                receipt.item_id = Some(item_id);
                receipt.item_revision = Some(1);
            }
            Edit {
                item_id,
                revision,
                text,
            } => {
                let next = revision
                    .checked_add(1)
                    .ok_or(PersistenceError::RequestConflict)?;
                let changed = tx.execute(
                    "UPDATE steering_pending_messages SET text = ?1, revision = ?2
                    WHERE session_id = ?3 AND item_id = ?4 AND revision = ?5",
                    params![
                        text,
                        sequence_to_sql(next)?,
                        session_bytes,
                        item_id,
                        sequence_to_sql(*revision)?
                    ],
                )?;
                if changed != 1 {
                    return Err(PersistenceError::RequestConflict);
                }
                receipt.item_id = Some(*item_id);
                receipt.item_revision = Some(next);
            }
            Remove { item_id, revision } => {
                let changed = tx.execute("DELETE FROM steering_pending_messages WHERE session_id = ?1 AND item_id = ?2 AND revision = ?3",
                    params![session_bytes, item_id, sequence_to_sql(*revision)?])?;
                if changed != 1 {
                    return Err(PersistenceError::RequestConflict);
                }
                receipt.item_id = Some(*item_id);
                receipt.item_revision = Some(*revision);
            }
            Pause => {
                tx.execute(
                    "UPDATE steering_queues SET paused = 1 WHERE session_id = ?1",
                    [session_bytes],
                )?;
            }
            Resume { run_id } => {
                tx.execute("UPDATE steering_queues SET paused = 0, target_run_id = ?2 WHERE session_id = ?1", params![session_bytes, run_id.as_bytes()])?;
            }
        }
        let changed = tx.execute(
            "UPDATE steering_queues SET revision = ?2 WHERE session_id = ?1",
            params![session_bytes, sequence_to_sql(revision)?],
        )?;
        if changed != 1 {
            return Err(PersistenceError::RequestConflict);
        }
        tx.execute(
            "INSERT INTO mutation_requests VALUES (?1, 18, ?2, ?3)",
            params![
                mutation.request_id.as_bytes(),
                sequence_to_sql(sequence)?,
                time_to_sql(now)?
            ],
        )?;
        let (change_kind, target_run_id, text) = match &mutation.change {
            Enqueue { run_id, text } => (1, Some(run_id.as_bytes()), Some(text)),
            Edit { text, .. } => (2, None, Some(text)),
            Remove { .. } => (3, None, None),
            Pause => (4, None, None),
            Resume { run_id } => (5, Some(run_id.as_bytes()), None),
        };
        tx.execute(
            "INSERT INTO steering_mutation_requests
             (request_id, session_id, operation_fingerprint, accepted_sequence,
              accepted_at_milliseconds, queue_revision, item_id, item_revision, actor,
              change_kind, target_run_id, text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9, ?10, ?11)",
            params![
                mutation.request_id.as_bytes(),
                session_bytes,
                fingerprint,
                sequence_to_sql(sequence)?,
                time_to_sql(now)?,
                sequence_to_sql(receipt.queue_revision)?,
                receipt.item_id,
                receipt.item_revision.map(sequence_to_sql).transpose()?,
                change_kind,
                target_run_id,
                text
            ],
        )?;
        tx.commit()?;
        Ok(receipt)
    }
}

fn read_unsigned(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    unsigned(row.get(index)?, index)
}

fn unsigned(value: i64, index: usize) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}
