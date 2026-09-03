BEGIN IMMEDIATE;

PRAGMA defer_foreign_keys = ON;

CREATE TABLE mutation_requests_v22 (
    request_id BLOB PRIMARY KEY NOT NULL CHECK (length(request_id) = 16),
    operation_kind INTEGER NOT NULL CHECK (operation_kind BETWEEN 1 AND 14),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;
INSERT INTO mutation_requests_v22 SELECT * FROM mutation_requests;
DROP TABLE mutation_requests;
ALTER TABLE mutation_requests_v22 RENAME TO mutation_requests;

CREATE TABLE delivery_events_v22 (
    event_id BLOB PRIMARY KEY NOT NULL CHECK (length(event_id) = 16),
    event_sequence INTEGER NOT NULL UNIQUE CHECK (event_sequence > 0),
    session_id BLOB NOT NULL CHECK (length(session_id) = 16),
    event_kind INTEGER NOT NULL CHECK (event_kind BETWEEN 1 AND 20),
    payload_version INTEGER NOT NULL CHECK (payload_version = 1),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;
INSERT INTO delivery_events_v22 SELECT * FROM delivery_events;
DROP TABLE delivery_events;
ALTER TABLE delivery_events_v22 RENAME TO delivery_events;
CREATE INDEX delivery_events_by_session
ON delivery_events (session_id, event_sequence);

CREATE TABLE session_delete_requests (
    request_id BLOB PRIMARY KEY NOT NULL REFERENCES mutation_requests(request_id),
    operation_fingerprint BLOB NOT NULL CHECK (length(operation_fingerprint) = 32),
    session_id BLOB NOT NULL UNIQUE CHECK (length(session_id) = 16),
    state INTEGER NOT NULL CHECK (state BETWEEN 1 AND 3),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0),
    delivery_event_id BLOB NOT NULL UNIQUE CHECK (length(delivery_event_id) = 16)
) STRICT, WITHOUT ROWID;

CREATE TABLE session_delete_attachments (
    delete_request_id BLOB NOT NULL REFERENCES session_delete_requests(request_id),
    attachment_id BLOB NOT NULL CHECK (length(attachment_id) = 16),
    PRIMARY KEY (delete_request_id, attachment_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE deleted_mutation_tombstones (
    request_id BLOB PRIMARY KEY NOT NULL CHECK (length(request_id) = 16),
    delete_request_id BLOB NOT NULL REFERENCES session_delete_requests(request_id),
    operation_kind INTEGER NOT NULL CHECK (operation_kind BETWEEN 1 AND 13),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;

PRAGMA user_version = 22;

COMMIT;
