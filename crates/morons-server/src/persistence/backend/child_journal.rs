use rusqlite::{OptionalExtension as _, params};
use sha2::{Digest as _, Sha256};

use super::{Backend, records::sequence_to_sql};
use crate::persistence::{ChildEntryKind as Kind, PersistenceError, ToolCallId};

const MAX_ENTRY_BYTES: usize = 16 * 1024 * 1024;

impl Backend {
    pub(crate) fn append_child_entry(
        &mut self,
        call_id: ToolCallId,
        child: u16,
        ordinal: u64,
        kind: Kind,
        payload: &[u8],
        previous: [u8; 32],
    ) -> Result<[u8; 32], PersistenceError> {
        if !(1..=3).contains(&child) || payload.is_empty() || payload.len() > MAX_ENTRY_BYTES {
            return Err(invalid());
        }
        let transaction = self.connection.transaction()?;
        let head: Option<(i64, i64, [u8; 32])> = transaction.query_row(
            "SELECT ordinal,kind,digest FROM child_journal WHERE call_id=?1 AND child=?2 ORDER BY ordinal DESC LIMIT 1",
            params![call_id.as_bytes(),child], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).optional()?;
        let (last, last_kind, digest) = head.unwrap_or((0, 0, [0; 32]));
        if last.checked_add(1) != Some(sequence_to_sql(ordinal)?)
            || previous != digest
            || !valid_transition(last_kind, kind as i64)
        {
            return Err(invalid());
        }
        if kind == Kind::Start {
            let active: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM task_model_bindings AS b JOIN runs AS r USING(run_id) WHERE b.call_id=?1 AND r.state=2 AND NOT EXISTS(SELECT 1 FROM tool_operation_facts AS f WHERE f.call_id=b.call_id AND f.fact_kind IN (3,4,5,6)))",
                [call_id.as_bytes()], |r|r.get(0),
            )?;
            if !active {
                return Err(invalid());
            }
            transaction.execute(
                "INSERT INTO child_runs(call_id,child,state) VALUES(?1,?2,1)",
                params![call_id.as_bytes(), child],
            )?;
        } else {
            let state: i64 = transaction.query_row(
                "SELECT state FROM child_runs WHERE call_id=?1 AND child=?2",
                params![call_id.as_bytes(), child],
                |r| r.get(0),
            )?;
            if state != 1 {
                return Err(invalid());
            }
        }
        let digest = entry_digest(call_id, child, ordinal, kind as i64, payload, previous);
        transaction.execute("INSERT INTO child_journal(call_id,child,ordinal,kind,payload,previous_digest,digest) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![call_id.as_bytes(),child,sequence_to_sql(ordinal)?,kind as i64,payload,previous,digest])?;
        if matches!(kind, Kind::Terminal | Kind::Interrupted) {
            transaction.execute(
                "UPDATE child_runs SET state=?3 WHERE call_id=?1 AND child=?2",
                params![
                    call_id.as_bytes(),
                    child,
                    if kind == Kind::Terminal { 2 } else { 3 }
                ],
            )?;
        }
        transaction.commit()?;
        Ok(digest)
    }

    pub(super) fn recover_child_journals(&mut self) -> Result<(), PersistenceError> {
        let children = {
            let mut statement = self
                .connection
                .prepare("SELECT call_id,child,state FROM child_runs ORDER BY call_id,child")?;
            statement
                .query_map([], |r| {
                    Ok((
                        r.get::<_, [u8; 16]>(0)?,
                        r.get::<_, u16>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (call, child, state) in children {
            let call = ToolCallId::from_bytes(call);
            let mut ordinal = 0_u64;
            let mut previous = [0; 32];
            let mut last_kind = 0;
            {
                let mut statement = self.connection.prepare("SELECT ordinal,kind,payload,previous_digest,digest FROM child_journal WHERE call_id=?1 AND child=?2 ORDER BY ordinal")?;
                let mut rows = statement.query(params![call.as_bytes(), child])?;
                while let Some(row) = rows.next()? {
                    let next = super::records::nonnegative_integer_from_row(row, 0)?;
                    let kind: i64 = row.get(1)?;
                    let payload: Vec<u8> = row.get(2)?;
                    let source: [u8; 32] = row.get(3)?;
                    let digest: [u8; 32] = row.get(4)?;
                    if ordinal.checked_add(1) != Some(next)
                        || previous != source
                        || !valid_transition(last_kind, kind)
                        || payload.is_empty()
                        || payload.len() > MAX_ENTRY_BYTES
                        || entry_digest(call, child, next, kind, &payload, source) != digest
                    {
                        return Err(invalid());
                    }
                    ordinal = next;
                    previous = digest;
                    last_kind = kind;
                }
            }
            if ordinal == 0
                || state
                    != match last_kind {
                        8 => 2,
                        9 => 3,
                        _ => 1,
                    }
            {
                return Err(invalid());
            }
            if state == 1 {
                self.append_child_entry(
                    call,
                    child,
                    ordinal.checked_add(1).ok_or_else(invalid)?,
                    Kind::Interrupted,
                    b"Interrupted on server recovery; effects may remain; nothing was retried",
                    previous,
                )?;
            }
        }
        Ok(())
    }
}

fn valid_transition(previous: i64, next: i64) -> bool {
    matches!(
        (previous, next),
        (0, 1)
            | (1..=7, 8 | 9)
            | (1 | 3 | 6 | 7, 2)
            | (2, 3)
            | (3 | 5, 4)
            | (4, 5)
            | (5, 6)
            | (3, 7)
    )
}

fn entry_digest(
    call: ToolCallId,
    child: u16,
    ordinal: u64,
    kind: i64,
    payload: &[u8],
    previous: [u8; 32],
) -> [u8; 32] {
    Sha256::new()
        .chain_update(b"morons.dev/child-journal/v1\0")
        .chain_update(call.as_bytes())
        .chain_update(child.to_be_bytes())
        .chain_update(ordinal.to_be_bytes())
        .chain_update(kind.to_be_bytes())
        .chain_update(previous)
        .chain_update(payload)
        .finalize()
        .into()
}

fn invalid() -> PersistenceError {
    PersistenceError::InvalidState {
        reason: "child journal integrity or lifecycle is invalid; nothing was retried",
    }
}
