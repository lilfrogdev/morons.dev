use super::{
    Backend,
    records::{
        current_time_milliseconds, load_mutation_operation, next_sequence, random_identifier,
        sequence_to_sql, time_to_sql,
    },
};
use crate::persistence::{ExportPlan, MutationRequestId, PersistenceError, SessionId};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};

const OPERATION_EXPORT: i64 = 10;

impl Backend {
    pub(super) fn recover_exports(&mut self) -> Result<(), PersistenceError> {
        self.connection
            .execute("UPDATE export_requests SET state=3 WHERE state=0", [])?;
        self.connection
            .execute("UPDATE export_requests SET state=4 WHERE state=1", [])?;
        Ok(())
    }
    pub(crate) fn prepare_export(
        &mut self,
        request_id: MutationRequestId,
        session_id: SessionId,
        generation_id: [u8; 16],
        destination_digest: [u8; 32],
    ) -> Result<ExportPlan, PersistenceError> {
        if let Some(plan) = load_plan(&self.connection, request_id)? {
            let stored: ([u8; 32], [u8; 32]) = self.connection.query_row(
                "SELECT fingerprint,destination_digest FROM export_requests WHERE request_id=?1",
                [&request_id.as_bytes()[..]],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if stored
                != (
                    fingerprint(session_id, generation_id, destination_digest),
                    destination_digest,
                )
            {
                return Err(PersistenceError::RequestConflict);
            }
            return Ok(plan);
        }
        if load_mutation_operation(&self.connection, request_id)?.is_some() {
            return Err(PersistenceError::RequestConflict);
        }
        let (workspace_id,active, busy):([u8;16],[u8;16],bool)=self.connection.query_row(
            "SELECT session.workspace_id, active.generation_id,
                    EXISTS(SELECT 1 FROM runs WHERE session_id=?1 AND state IN(1,2))
             FROM sessions AS session JOIN active_worktree_generations AS active ON active.workspace_id=session.workspace_id
             WHERE session.session_id=?1",[&session_id.as_bytes()[..]],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        if busy {
            return Err(PersistenceError::WorkspaceBusy);
        }
        if active != generation_id {
            return Err(PersistenceError::ReviewCursorStale);
        }
        let operation_id = random_identifier()?;
        let now = current_time_milliseconds()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let seq = next_sequence(&tx)?;
        tx.execute("INSERT INTO mutation_requests(request_id,operation_kind,accepted_sequence,accepted_at_milliseconds)VALUES(?1,?2,?3,?4)",params![&request_id.as_bytes()[..],OPERATION_EXPORT,sequence_to_sql(seq)?,time_to_sql(now)?])?;
        tx.execute("INSERT INTO export_requests(request_id,fingerprint,destination_digest,session_id,workspace_id,generation_id,operation_id,state,accepted_sequence,accepted_at_milliseconds)VALUES(?1,?2,?3,?4,?5,?6,?7,0,?8,?9)",params![&request_id.as_bytes()[..],&fingerprint(session_id,generation_id,destination_digest)[..],&destination_digest[..],&session_id.as_bytes()[..],&workspace_id[..],&generation_id[..],&operation_id[..],sequence_to_sql(seq)?,time_to_sql(now)?])?;
        tx.commit()?;
        Ok(ExportPlan {
            request_id,
            session_id,
            workspace_id,
            generation_id,
            operation_id,
            state: 0,
        })
    }
    pub(crate) fn dispatch_export(
        &mut self,
        plan: ExportPlan,
    ) -> Result<ExportPlan, PersistenceError> {
        if plan.state != 0 {
            return Ok(plan);
        }
        let changed = self.connection.execute(
            "UPDATE export_requests SET state=1 WHERE request_id=?1 AND state=0",
            [&plan.request_id.as_bytes()[..]],
        )?;
        if changed != 1 {
            return Err(PersistenceError::InvalidState {
                reason: "export dispatch state changed",
            });
        }
        Ok(ExportPlan { state: 1, ..plan })
    }
    pub(crate) fn export_summary(
        &self,
        request_id: MutationRequestId,
    ) -> Result<morons_protocol::ExportSummary, PersistenceError> {
        self.connection.query_row("SELECT file_count,directory_count,logical_bytes FROM export_requests WHERE request_id=?1 AND state=2",[&request_id.as_bytes()[..]],|r|{let f:i64=r.get(0)?;let d:i64=r.get(1)?;let b:i64=r.get(2)?;Ok(morons_protocol::ExportSummary{file_count:u64::try_from(f).map_err(|_|rusqlite::Error::IntegralValueOutOfRange(0,f))?,directory_count:u64::try_from(d).map_err(|_|rusqlite::Error::IntegralValueOutOfRange(1,d))?,logical_bytes:u64::try_from(b).map_err(|_|rusqlite::Error::IntegralValueOutOfRange(2,b))?})}).map_err(Into::into)
    }
    pub(crate) fn complete_export(
        &mut self,
        plan: ExportPlan,
        outcome: crate::persistence::RepositoryImportOutcome,
    ) -> Result<morons_protocol::ExportSummary, PersistenceError> {
        let changed=self.connection.execute("UPDATE export_requests SET state=2,file_count=?2,directory_count=?3,logical_bytes=?4 WHERE request_id=?1 AND state=1",params![&plan.request_id.as_bytes()[..],sequence_to_sql(outcome.file_count)?,sequence_to_sql(outcome.directory_count)?,sequence_to_sql(outcome.logical_bytes)?])?;
        if changed != 1 {
            return Err(PersistenceError::InvalidState {
                reason: "export completion state changed",
            });
        }
        Ok(morons_protocol::ExportSummary {
            file_count: outcome.file_count,
            directory_count: outcome.directory_count,
            logical_bytes: outcome.logical_bytes,
        })
    }
}
fn load_plan(
    c: &rusqlite::Connection,
    id: MutationRequestId,
) -> Result<Option<ExportPlan>, PersistenceError> {
    c.query_row("SELECT request_id,session_id,workspace_id,generation_id,operation_id,state FROM export_requests WHERE request_id=?1",[&id.as_bytes()[..]],|r|{let state:i64=r.get(5)?;Ok(ExportPlan{request_id:MutationRequestId::from_bytes(r.get(0)?),session_id:SessionId::from_bytes(r.get(1)?),workspace_id:r.get(2)?,generation_id:r.get(3)?,operation_id:r.get(4)?,state:u8::try_from(state).map_err(|_|rusqlite::Error::IntegralValueOutOfRange(5,state))?})}).optional().map_err(Into::into)
}
fn fingerprint(session: SessionId, generation: [u8; 16], destination: [u8; 32]) -> [u8; 32] {
    let mut d = Sha256::new();
    d.update(b"morons.dev/export/v1\0");
    d.update(session.as_bytes());
    d.update(generation);
    d.update(destination);
    d.finalize().into()
}
