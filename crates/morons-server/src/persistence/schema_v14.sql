BEGIN IMMEDIATE;

PRAGMA defer_foreign_keys = ON;

CREATE TABLE mutation_requests_v14 (
    request_id BLOB PRIMARY KEY NOT NULL CHECK (length(request_id) = 16),
    operation_kind INTEGER NOT NULL CHECK (operation_kind BETWEEN 1 AND 11),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;
INSERT INTO mutation_requests_v14 SELECT * FROM mutation_requests;
DROP TABLE mutation_requests;
ALTER TABLE mutation_requests_v14 RENAME TO mutation_requests;

CREATE TABLE local_commands (
    command_id BLOB PRIMARY KEY NOT NULL CHECK (length(command_id) = 16),
    request_id BLOB NOT NULL UNIQUE REFERENCES mutation_requests(request_id),
    operation_fingerprint BLOB NOT NULL CHECK (length(operation_fingerprint) = 32),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    command_text TEXT NOT NULL CHECK (length(CAST(command_text AS BLOB)) BETWEEN 1 AND 65536),
    context_visible INTEGER NOT NULL CHECK (context_visible IN (0, 1)),
    state INTEGER NOT NULL CHECK (state BETWEEN 1 AND 5),
    cancellation_requested INTEGER NOT NULL CHECK (cancellation_requested IN (0, 1)),
    result_payload BLOB CHECK (result_payload IS NULL OR length(result_payload) BETWEEN 2 AND 524288),
    entry_sequence INTEGER CHECK (entry_sequence IS NULL OR entry_sequence > 0),
    message_id BLOB UNIQUE CHECK (message_id IS NULL OR length(message_id) = 16),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    updated_sequence INTEGER NOT NULL UNIQUE CHECK (updated_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0),
    updated_at_milliseconds INTEGER NOT NULL CHECK (updated_at_milliseconds >= accepted_at_milliseconds),
    accepted_event_id BLOB NOT NULL UNIQUE CHECK (length(accepted_event_id) = 16),
    delivery_event_id BLOB UNIQUE CHECK (delivery_event_id IS NULL OR length(delivery_event_id) = 16),
    CHECK (
        (state IN (1, 2) AND result_payload IS NULL AND entry_sequence IS NULL
         AND message_id IS NULL AND delivery_event_id IS NULL)
        OR
        (state BETWEEN 3 AND 5 AND result_payload IS NOT NULL AND entry_sequence IS NOT NULL
         AND message_id IS NOT NULL AND delivery_event_id IS NOT NULL)
    ),
    UNIQUE (session_id, entry_sequence)
) STRICT, WITHOUT ROWID;
CREATE UNIQUE INDEX local_commands_one_active_per_session
ON local_commands (session_id) WHERE state IN (1, 2);
CREATE INDEX local_commands_by_session_entry
ON local_commands (session_id, entry_sequence) WHERE entry_sequence IS NOT NULL;

CREATE TABLE local_command_cancellations (
    request_id BLOB PRIMARY KEY NOT NULL REFERENCES mutation_requests(request_id),
    operation_fingerprint BLOB NOT NULL CHECK (length(operation_fingerprint) = 32),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    command_id BLOB NOT NULL REFERENCES local_commands(command_id),
    intent_applied INTEGER NOT NULL CHECK (intent_applied IN (0, 1)),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;

CREATE TABLE local_command_audit_facts (
    audit_id BLOB PRIMARY KEY NOT NULL CHECK (length(audit_id) = 16),
    audit_sequence INTEGER NOT NULL UNIQUE CHECK (audit_sequence > 0),
    command_id BLOB NOT NULL REFERENCES local_commands(command_id),
    request_id BLOB REFERENCES mutation_requests(request_id),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    audit_kind INTEGER NOT NULL CHECK (audit_kind BETWEEN 1 AND 4),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;
CREATE INDEX local_command_audit_by_session
ON local_command_audit_facts (session_id, audit_sequence);

CREATE TABLE delivery_events_v14 (
    event_id BLOB PRIMARY KEY NOT NULL CHECK (length(event_id) = 16),
    event_sequence INTEGER NOT NULL UNIQUE CHECK (event_sequence > 0),
    session_id BLOB REFERENCES session_created_facts(session_id),
    event_kind INTEGER NOT NULL CHECK (event_kind BETWEEN 1 AND 17),
    payload_version INTEGER NOT NULL CHECK (payload_version = 1),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;
INSERT INTO delivery_events_v14 SELECT * FROM delivery_events;
DROP TABLE delivery_events;
ALTER TABLE delivery_events_v14 RENAME TO delivery_events;
CREATE INDEX delivery_events_by_session
ON delivery_events (session_id, event_sequence) WHERE session_id IS NOT NULL;

PRAGMA user_version = 14;

COMMIT;
