use rusqlite::params;

use super::{
    Backend,
    records::{load_session, nonnegative_integer_from_row, session_from_row, session_from_row_at},
};
use crate::persistence::{
    PersistenceError, Session, SessionCatalogEvent, SessionCatalogEventCursor,
    SessionCatalogEventPage, SessionId, SessionListCursor, SessionPage,
};

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
        let current_event_sequence = session_catalog_high_water(self)?;
        let (snapshot_event_sequence, after_created_sequence) =
            cursor.map_or((current_event_sequence, 0), |cursor| {
                (
                    cursor.snapshot_event_sequence(),
                    cursor.after_created_sequence(),
                )
            });
        if snapshot_event_sequence > current_event_sequence
            || after_created_sequence > snapshot_event_sequence
        {
            return Err(PersistenceError::InvalidInput {
                reason: "a session list cursor is outside the available snapshot",
            });
        }

        let query_limit = i64::from(limit) + 1;
        let mut statement = self.connection.prepare(
            "SELECT
                session.session_id,
                session.workspace_id,
                session.display_name,
                session.working_directory,
                session.created_sequence,
                session.updated_sequence,
                session.created_at_milliseconds
            FROM sessions AS session
            INNER JOIN session_created_facts AS fact ON fact.session_id = session.session_id
            WHERE fact.fact_sequence <= ?1
              AND session.created_sequence > ?2
            ORDER BY session.created_sequence, session.session_id
            LIMIT ?3",
        )?;
        let mut sessions = statement
            .query_map(
                params![
                    sequence_to_sql(snapshot_event_sequence)?,
                    sequence_to_sql(after_created_sequence)?,
                    query_limit,
                ],
                session_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;

        let has_more = sessions.len() > usize::from(limit);
        if has_more {
            sessions.pop();
        }
        let next_cursor = if has_more {
            sessions.last().map(|session| {
                SessionListCursor::new(snapshot_event_sequence, session.created_sequence)
            })
        } else {
            None
        };
        Ok(SessionPage {
            sessions,
            next_cursor,
            catalog_cursor: SessionCatalogEventCursor::from_sequence(snapshot_event_sequence),
        })
    }

    pub(crate) fn read_session_catalog_events(
        &self,
        cursor: SessionCatalogEventCursor,
        limit: u16,
    ) -> Result<SessionCatalogEventPage, PersistenceError> {
        let high_water = session_catalog_high_water(self)?;
        if cursor.sequence() > high_water {
            return Err(PersistenceError::InvalidInput {
                reason: "a session catalog cursor is ahead of the durable event stream",
            });
        }

        let mut statement = self.connection.prepare(
            "SELECT
                event.event_sequence,
                fact.session_id,
                fact.workspace_id,
                fact.display_name,
                fact.working_directory,
                fact.accepted_sequence,
                fact.fact_sequence,
                fact.created_at_milliseconds
            FROM delivery_events AS event
            INNER JOIN session_created_facts AS fact ON fact.delivery_event_id = event.event_id
            WHERE event.event_sequence > ?1
              AND event.event_sequence <= ?2
              AND event.event_kind = 1
              AND event.payload_version = 1
            ORDER BY event.event_sequence
            LIMIT ?3",
        )?;
        let events = statement
            .query_map(
                params![
                    sequence_to_sql(cursor.sequence())?,
                    sequence_to_sql(high_water)?,
                    i64::from(limit),
                ],
                |row| {
                    Ok(SessionCatalogEvent {
                        cursor: SessionCatalogEventCursor::from_sequence(
                            nonnegative_integer_from_row(row, 0)?,
                        ),
                        session: session_from_row_at(row, 1)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SessionCatalogEventPage {
            events,
            high_water: SessionCatalogEventCursor::from_sequence(high_water),
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

fn session_catalog_high_water(backend: &Backend) -> Result<u64, PersistenceError> {
    let sequence = backend.connection.query_row(
        "SELECT COALESCE(MAX(event_sequence), 0)
         FROM delivery_events
         WHERE event_kind = 1 AND payload_version = 1",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    u64::try_from(sequence).map_err(|_| PersistenceError::InvalidState {
        reason: "the session catalog high water is outside its supported range",
    })
}

fn sequence_to_sql(sequence: u64) -> Result<i64, PersistenceError> {
    i64::try_from(sequence).map_err(|_| PersistenceError::InvalidInput {
        reason: "a session cursor exceeds SQLite's integer range",
    })
}
