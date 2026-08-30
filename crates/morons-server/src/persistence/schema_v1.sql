BEGIN IMMEDIATE;

PRAGMA application_id = 1297044046;

CREATE TABLE logical_sequences (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    next_value INTEGER NOT NULL CHECK (next_value > 0)
) STRICT, WITHOUT ROWID;

INSERT INTO logical_sequences (singleton, next_value) VALUES (1, 1);

CREATE TABLE session_creation_requests (
    request_id BLOB PRIMARY KEY NOT NULL CHECK (length(request_id) = 16),
    operation_fingerprint BLOB NOT NULL CHECK (length(operation_fingerprint) = 32),
    session_id BLOB NOT NULL UNIQUE CHECK (length(session_id) = 16),
    workspace_id BLOB NOT NULL UNIQUE CHECK (length(workspace_id) = 16),
    display_name TEXT CHECK (
        display_name IS NULL OR length(CAST(display_name AS BLOB)) BETWEEN 1 AND 256
    ),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0),
    state INTEGER NOT NULL CHECK (state IN (0, 1, 2))
) STRICT, WITHOUT ROWID;

CREATE INDEX session_creation_requests_by_state
ON session_creation_requests (state, accepted_sequence);

CREATE TABLE workspace_operation_facts (
    fact_id BLOB PRIMARY KEY NOT NULL CHECK (length(fact_id) = 16),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    request_id BLOB NOT NULL REFERENCES session_creation_requests(request_id),
    workspace_id BLOB NOT NULL CHECK (length(workspace_id) = 16),
    operation_kind INTEGER NOT NULL CHECK (operation_kind IN (1, 2)),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    UNIQUE (request_id, operation_kind)
) STRICT, WITHOUT ROWID;

CREATE TABLE session_created_facts (
    fact_id BLOB PRIMARY KEY NOT NULL CHECK (length(fact_id) = 16),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    request_id BLOB NOT NULL UNIQUE REFERENCES session_creation_requests(request_id),
    session_id BLOB NOT NULL UNIQUE CHECK (length(session_id) = 16),
    workspace_id BLOB NOT NULL UNIQUE CHECK (length(workspace_id) = 16),
    display_name TEXT CHECK (
        display_name IS NULL OR length(CAST(display_name AS BLOB)) BETWEEN 1 AND 256
    ),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    delivery_event_id BLOB NOT NULL UNIQUE CHECK (length(delivery_event_id) = 16)
) STRICT, WITHOUT ROWID;

CREATE TABLE sessions (
    session_id BLOB PRIMARY KEY NOT NULL REFERENCES session_created_facts(session_id),
    workspace_id BLOB NOT NULL UNIQUE CHECK (length(workspace_id) = 16),
    display_name TEXT CHECK (
        display_name IS NULL OR length(CAST(display_name AS BLOB)) BETWEEN 1 AND 256
    ),
    created_sequence INTEGER NOT NULL UNIQUE CHECK (created_sequence > 0),
    updated_sequence INTEGER NOT NULL CHECK (updated_sequence >= created_sequence),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    lifecycle INTEGER NOT NULL CHECK (lifecycle = 1)
) STRICT, WITHOUT ROWID;

CREATE INDEX sessions_by_creation
ON sessions (created_sequence, session_id);

CREATE TABLE delivery_events (
    event_id BLOB PRIMARY KEY NOT NULL CHECK (length(event_id) = 16),
    event_sequence INTEGER NOT NULL UNIQUE CHECK (event_sequence > 0),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    event_kind INTEGER NOT NULL CHECK (event_kind = 1),
    payload_version INTEGER NOT NULL CHECK (payload_version = 1),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;

CREATE INDEX delivery_events_by_session
ON delivery_events (session_id, event_sequence);

CREATE TABLE audit_facts (
    audit_id BLOB PRIMARY KEY NOT NULL CHECK (length(audit_id) = 16),
    audit_sequence INTEGER NOT NULL UNIQUE CHECK (audit_sequence > 0),
    request_id BLOB NOT NULL REFERENCES session_creation_requests(request_id),
    session_id BLOB NOT NULL CHECK (length(session_id) = 16),
    audit_kind INTEGER NOT NULL CHECK (audit_kind IN (1, 2, 3)),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    UNIQUE (request_id, audit_kind)
) STRICT, WITHOUT ROWID;

PRAGMA user_version = 1;

COMMIT;
