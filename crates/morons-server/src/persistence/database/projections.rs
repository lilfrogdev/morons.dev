use rusqlite::Connection;

use super::validate_integrity;
use crate::persistence::{
    PersistenceError,
    types::{create_session_fingerprint, validate_display_name},
};

pub(super) fn repair(connection: &mut Connection) -> Result<(), PersistenceError> {
    validate_request_payloads(connection)?;
    let requests_invalid: bool = connection.query_row(
        "SELECT EXISTS (
            SELECT 1
            FROM session_creation_requests AS request
            LEFT JOIN session_created_facts AS fact ON fact.request_id = request.request_id
            WHERE (fact.request_id IS NOT NULL) != (request.state = 2)
               OR (fact.request_id IS NOT NULL AND (
                    fact.session_id IS NOT request.session_id
                    OR fact.workspace_id IS NOT request.workspace_id
                    OR fact.display_name IS NOT request.display_name
                    OR fact.accepted_sequence IS NOT request.accepted_sequence
                    OR fact.created_at_milliseconds IS NOT request.accepted_at_milliseconds
               ))
               OR (SELECT COUNT(*) FROM workspace_operation_facts AS workspace
                   WHERE workspace.request_id = request.request_id
                     AND workspace.operation_kind = 1) != CASE WHEN request.state >= 1 THEN 1 ELSE 0 END
               OR (SELECT COUNT(*) FROM workspace_operation_facts AS workspace
                   WHERE workspace.request_id = request.request_id
                     AND workspace.operation_kind = 2) != CASE WHEN request.state = 2 THEN 1 ELSE 0 END
               OR EXISTS (
                    SELECT 1 FROM workspace_operation_facts AS workspace
                    WHERE workspace.request_id = request.request_id
                      AND workspace.workspace_id IS NOT request.workspace_id
               )
               OR (SELECT COUNT(*) FROM audit_facts AS audit
                   WHERE audit.request_id = request.request_id
                     AND audit.audit_kind = 1) != 1
               OR (SELECT COUNT(*) FROM audit_facts AS audit
                   WHERE audit.request_id = request.request_id
                     AND audit.audit_kind = 2) != CASE WHEN request.state >= 1 THEN 1 ELSE 0 END
               OR (SELECT COUNT(*) FROM audit_facts AS audit
                   WHERE audit.request_id = request.request_id
                     AND audit.audit_kind = 3) != CASE WHEN request.state = 2 THEN 1 ELSE 0 END
               OR EXISTS (
                    SELECT 1 FROM audit_facts AS audit
                    WHERE audit.request_id = request.request_id
                      AND audit.session_id IS NOT request.session_id
               )
        )",
        [],
        |row| row.get(0),
    )?;
    if requests_invalid {
        return Err(PersistenceError::InvalidState {
            reason: "session creation facts conflict with idempotency state",
        });
    }

    let sessions_invalid: bool = connection.query_row(
        "SELECT EXISTS (
            SELECT 1
            FROM sessions AS session
            LEFT JOIN session_created_facts AS fact ON fact.session_id = session.session_id
            WHERE fact.session_id IS NULL
               OR session.workspace_id IS NOT fact.workspace_id
               OR session.display_name IS NOT fact.display_name
               OR session.created_sequence IS NOT fact.accepted_sequence
               OR session.updated_sequence IS NOT fact.fact_sequence
               OR session.created_at_milliseconds IS NOT fact.created_at_milliseconds
               OR session.lifecycle != 1
            UNION ALL
            SELECT 1
            FROM session_created_facts AS fact
            LEFT JOIN sessions AS session ON session.session_id = fact.session_id
            WHERE session.session_id IS NULL
        )",
        [],
        |row| row.get(0),
    )?;
    let delivery_events_invalid: bool = connection.query_row(
        "SELECT EXISTS (
            SELECT 1
            FROM delivery_events AS event
            LEFT JOIN session_created_facts AS fact ON fact.delivery_event_id = event.event_id
            WHERE fact.delivery_event_id IS NULL
               OR event.event_sequence IS NOT fact.fact_sequence
               OR event.session_id IS NOT fact.session_id
               OR event.event_kind != 1
               OR event.payload_version != 1
               OR event.created_at_milliseconds IS NOT fact.created_at_milliseconds
            UNION ALL
            SELECT 1
            FROM session_created_facts AS fact
            LEFT JOIN delivery_events AS event ON event.event_id = fact.delivery_event_id
            WHERE event.event_id IS NULL
        )",
        [],
        |row| row.get(0),
    )?;

    if !sessions_invalid && !delivery_events_invalid {
        return Ok(());
    }

    let transaction = connection.transaction()?;
    if sessions_invalid {
        transaction.execute("DELETE FROM sessions", [])?;
        transaction.execute(
            "INSERT INTO sessions (
                session_id,
                workspace_id,
                display_name,
                created_sequence,
                updated_sequence,
                created_at_milliseconds,
                lifecycle
            )
            SELECT
                session_id,
                workspace_id,
                display_name,
                accepted_sequence,
                fact_sequence,
                created_at_milliseconds,
                1
            FROM session_created_facts",
            [],
        )?;
    }
    if delivery_events_invalid {
        transaction.execute("DELETE FROM delivery_events", [])?;
        transaction.execute(
            "INSERT INTO delivery_events (
                event_id,
                event_sequence,
                session_id,
                event_kind,
                payload_version,
                created_at_milliseconds
            )
            SELECT
                delivery_event_id,
                fact_sequence,
                session_id,
                1,
                1,
                created_at_milliseconds
            FROM session_created_facts",
            [],
        )?;
    }
    transaction.commit()?;
    validate_integrity(connection)
}

fn validate_request_payloads(connection: &Connection) -> Result<(), PersistenceError> {
    let mut statement = connection
        .prepare("SELECT operation_fingerprint, display_name FROM session_creation_requests")?;
    let requests = statement
        .query_map([], |row| {
            Ok((row.get::<_, [u8; 32]>(0)?, row.get::<_, Option<String>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (fingerprint, display_name) in requests {
        if validate_display_name(display_name.as_deref()).is_err()
            || fingerprint != create_session_fingerprint(display_name.as_deref())
        {
            return Err(PersistenceError::InvalidState {
                reason: "a persisted session creation request has invalid canonical input",
            });
        }
    }
    Ok(())
}
