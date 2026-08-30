use rusqlite::params;

use super::{
    Backend,
    records::{load_session, session_from_row},
};
use crate::persistence::{PersistenceError, Session, SessionId, SessionListCursor, SessionPage};

impl Backend {
    pub(crate) fn get_session(
        &self,
        session_id: SessionId,
    ) -> Result<Option<Session>, PersistenceError> {
        load_session(&self.connection, session_id)
    }

    pub(crate) fn list_sessions(
        &self,
        cursor: Option<SessionListCursor>,
        limit: u16,
    ) -> Result<SessionPage, PersistenceError> {
        let after_sequence = cursor.map_or(Ok(0_i64), |cursor| {
            i64::try_from(cursor.sequence()).map_err(|_| PersistenceError::InvalidInput {
                reason: "a session list cursor exceeds the supported sequence range",
            })
        })?;
        let query_limit = i64::from(limit) + 1;
        let mut statement = self.connection.prepare(
            "SELECT
                session_id,
                workspace_id,
                display_name,
                created_sequence,
                updated_sequence,
                created_at_milliseconds
            FROM sessions
            WHERE created_sequence > ?1
            ORDER BY created_sequence, session_id
            LIMIT ?2",
        )?;
        let mut sessions = statement
            .query_map(params![after_sequence, query_limit], session_from_row)?
            .collect::<Result<Vec<_>, _>>()?;

        let has_more = sessions.len() > usize::from(limit);
        if has_more {
            sessions.pop();
        }
        let next_cursor = if has_more {
            sessions
                .last()
                .map(|session| SessionListCursor::from_sequence(session.created_sequence))
        } else {
            None
        };
        Ok(SessionPage {
            sessions,
            next_cursor,
        })
    }

    pub(super) fn load_required_session(
        &self,
        session_id: SessionId,
    ) -> Result<Session, PersistenceError> {
        load_session(&self.connection, session_id)?.ok_or(PersistenceError::InvalidState {
            reason: "a completed session is missing its current-state projection",
        })
    }
}
