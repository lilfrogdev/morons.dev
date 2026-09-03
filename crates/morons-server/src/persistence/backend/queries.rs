use rusqlite::params;

use super::{
    Backend,
    records::{load_session, nonnegative_integer_from_row, session_from_row, session_from_row_at},
};
use crate::persistence::{
    PersistenceError, Session, SessionCatalogEvent, SessionCatalogEventCursor,
    SessionCatalogEventKind, SessionCatalogEventPage, SessionId, SessionListCursor, SessionPage,
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
                fact.session_id,
                fact.workspace_id,
                COALESCE((
                    SELECT rename.display_name FROM session_rename_requests AS rename
                    WHERE rename.session_id = fact.session_id
                      AND rename.accepted_sequence <= ?1
                    ORDER BY rename.accepted_sequence DESC LIMIT 1
                ), fact.display_name),
                fact.working_directory,
                fact.accepted_sequence,
                MAX(fact.fact_sequence, COALESCE((
                    SELECT MAX(rename.accepted_sequence)
                    FROM session_rename_requests AS rename
                    WHERE rename.session_id = fact.session_id
                      AND rename.accepted_sequence <= ?1
                ), 0), COALESCE((
                    SELECT MAX(archive.accepted_sequence)
                    FROM session_archive_requests AS archive
                    WHERE archive.session_id = fact.session_id
                      AND archive.state = 2
                      AND archive.accepted_sequence <= ?1
                ), 0)),
                fact.created_at_milliseconds,
                COALESCE((
                    SELECT archive.archived FROM session_archive_requests AS archive
                    WHERE archive.session_id = fact.session_id
                      AND archive.state = 2
                      AND archive.accepted_sequence <= ?1
                    ORDER BY archive.accepted_sequence DESC LIMIT 1
                ), 0)
            FROM session_created_facts AS fact
            WHERE fact.fact_sequence <= ?1
              AND fact.accepted_sequence > ?2
            ORDER BY fact.accepted_sequence, fact.session_id
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
                event.event_kind,
                fact.session_id,
                fact.workspace_id,
                COALESCE((
                    SELECT rename.display_name FROM session_rename_requests AS rename
                    WHERE rename.session_id = fact.session_id
                      AND rename.accepted_sequence <= event.event_sequence
                    ORDER BY rename.accepted_sequence DESC LIMIT 1
                ), fact.display_name),
                fact.working_directory,
                fact.accepted_sequence,
                MAX(fact.fact_sequence, COALESCE((
                    SELECT MAX(rename.accepted_sequence)
                    FROM session_rename_requests AS rename
                    WHERE rename.session_id = fact.session_id
                      AND rename.accepted_sequence <= event.event_sequence
                ), 0), COALESCE((
                    SELECT MAX(archive.accepted_sequence)
                    FROM session_archive_requests AS archive
                    WHERE archive.session_id = fact.session_id
                      AND archive.state = 2
                      AND archive.accepted_sequence <= event.event_sequence
                ), 0)),
                fact.created_at_milliseconds,
                COALESCE((
                    SELECT archive.archived FROM session_archive_requests AS archive
                    WHERE archive.session_id = fact.session_id
                      AND archive.state = 2
                      AND archive.accepted_sequence <= event.event_sequence
                    ORDER BY archive.accepted_sequence DESC LIMIT 1
                ), 0)
            FROM delivery_events AS event
            INNER JOIN session_created_facts AS fact ON fact.session_id = event.session_id
            WHERE event.event_sequence > ?1
              AND event.event_sequence <= ?2
              AND event.event_kind IN (1, 18, 19)
              AND event.payload_version = 1
            ORDER BY event.event_sequence
            LIMIT ?3",
        )?;
        let mut events = statement
            .query_map(
                params![
                    sequence_to_sql(cursor.sequence())?,
                    sequence_to_sql(high_water)?,
                    i64::from(limit),
                ],
                |row| {
                    let session = session_from_row_at(row, 2)?;
                    Ok(SessionCatalogEvent {
                        cursor: SessionCatalogEventCursor::from_sequence(
                            nonnegative_integer_from_row(row, 0)?,
                        ),
                        kind: if row.get::<_, i64>(1)? == 1 {
                            SessionCatalogEventKind::Created(session)
                        } else {
                            SessionCatalogEventKind::Changed(session)
                        },
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let mut deleted_statement = self.connection.prepare(
            "SELECT event.event_sequence, event.session_id
             FROM delivery_events AS event
             INNER JOIN session_delete_requests AS deletion
               ON deletion.delivery_event_id = event.event_id
             WHERE event.event_sequence > ?1
               AND event.event_sequence <= ?2
               AND event.event_kind = 20
               AND event.payload_version = 1
               AND deletion.state = 3
             ORDER BY event.event_sequence
             LIMIT ?3",
        )?;
        events.extend(
            deleted_statement
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
                            kind: SessionCatalogEventKind::Removed(SessionId::from_bytes(
                                row.get(1)?,
                            )),
                        })
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?,
        );
        events.sort_by_key(|event| event.cursor);
        events.truncate(usize::from(limit));
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
         WHERE event_kind IN (1, 18, 19, 20) AND payload_version = 1",
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
