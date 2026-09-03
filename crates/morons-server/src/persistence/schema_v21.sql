BEGIN IMMEDIATE;

PRAGMA defer_foreign_keys = ON;

CREATE TABLE mutation_requests_v21 (
    request_id BLOB PRIMARY KEY NOT NULL CHECK (length(request_id) = 16),
    operation_kind INTEGER NOT NULL CHECK (operation_kind BETWEEN 1 AND 13),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;
INSERT INTO mutation_requests_v21 SELECT * FROM mutation_requests;
DROP TABLE mutation_requests;
ALTER TABLE mutation_requests_v21 RENAME TO mutation_requests;

CREATE TABLE delivery_events_v21 (
    event_id BLOB PRIMARY KEY NOT NULL CHECK (length(event_id) = 16),
    event_sequence INTEGER NOT NULL UNIQUE CHECK (event_sequence > 0),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    event_kind INTEGER NOT NULL CHECK (event_kind BETWEEN 1 AND 19),
    payload_version INTEGER NOT NULL CHECK (payload_version = 1),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;
INSERT INTO delivery_events_v21 SELECT * FROM delivery_events;
DROP TABLE delivery_events;
ALTER TABLE delivery_events_v21 RENAME TO delivery_events;
CREATE INDEX delivery_events_by_session
ON delivery_events (session_id, event_sequence);

CREATE TABLE session_archive_requests (
    request_id BLOB PRIMARY KEY NOT NULL REFERENCES mutation_requests(request_id),
    operation_fingerprint BLOB NOT NULL CHECK (length(operation_fingerprint) = 32),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    archived INTEGER NOT NULL CHECK (archived IN (0, 1)),
    state INTEGER NOT NULL CHECK (state IN (1, 2)),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0),
    delivery_event_id BLOB NOT NULL UNIQUE CHECK (length(delivery_event_id) = 16)
) STRICT, WITHOUT ROWID;
CREATE INDEX session_archive_requests_by_session
ON session_archive_requests (session_id, accepted_sequence);

ALTER TABLE sessions
ADD COLUMN archived INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0, 1));

PRAGMA user_version = 21;

COMMIT;
