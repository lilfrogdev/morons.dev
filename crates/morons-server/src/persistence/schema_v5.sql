BEGIN IMMEDIATE;

PRAGMA defer_foreign_keys = ON;

CREATE TABLE mutation_requests_v5 (
    request_id BLOB PRIMARY KEY NOT NULL CHECK (length(request_id) = 16),
    operation_kind INTEGER NOT NULL CHECK (operation_kind IN (1, 2, 3, 4, 5, 6, 7)),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;

INSERT INTO mutation_requests_v5 (
    request_id,
    operation_kind,
    accepted_sequence,
    accepted_at_milliseconds
)
SELECT request_id, operation_kind, accepted_sequence, accepted_at_milliseconds
FROM mutation_requests;

DROP TABLE mutation_requests;
ALTER TABLE mutation_requests_v5 RENAME TO mutation_requests;

CREATE TABLE delivery_events_v5 (
    event_id BLOB PRIMARY KEY NOT NULL CHECK (length(event_id) = 16),
    event_sequence INTEGER NOT NULL UNIQUE CHECK (event_sequence > 0),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    event_kind INTEGER NOT NULL CHECK (event_kind BETWEEN 1 AND 11),
    payload_version INTEGER NOT NULL CHECK (payload_version = 1),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;

INSERT INTO delivery_events_v5 (
    event_id,
    event_sequence,
    session_id,
    event_kind,
    payload_version,
    created_at_milliseconds
)
SELECT event_id, event_sequence, session_id, event_kind, payload_version, created_at_milliseconds
FROM delivery_events;

DROP TABLE delivery_events;
ALTER TABLE delivery_events_v5 RENAME TO delivery_events;

CREATE INDEX delivery_events_by_session
ON delivery_events (session_id, event_sequence);

CREATE TABLE repository_import_requests (
    request_id BLOB PRIMARY KEY NOT NULL REFERENCES mutation_requests(request_id),
    operation_fingerprint BLOB NOT NULL CHECK (length(operation_fingerprint) = 32),
    source_path_digest BLOB NOT NULL CHECK (length(source_path_digest) = 32),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    workspace_id BLOB NOT NULL CHECK (length(workspace_id) = 16),
    operation_id BLOB NOT NULL UNIQUE CHECK (length(operation_id) = 16),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0),
    state INTEGER NOT NULL CHECK (state BETWEEN 0 AND 4),
    file_count INTEGER CHECK (file_count IS NULL OR file_count >= 0),
    directory_count INTEGER CHECK (directory_count IS NULL OR directory_count >= 0),
    logical_bytes INTEGER CHECK (logical_bytes IS NULL OR logical_bytes >= 0),
    manifest_digest BLOB CHECK (manifest_digest IS NULL OR length(manifest_digest) = 32),
    CHECK (
        (state = 2 AND file_count IS NOT NULL AND directory_count IS NOT NULL
                   AND logical_bytes IS NOT NULL AND manifest_digest IS NOT NULL)
        OR
        (state != 2 AND file_count IS NULL AND directory_count IS NULL
                    AND logical_bytes IS NULL AND manifest_digest IS NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE INDEX repository_import_requests_by_state
ON repository_import_requests (state, accepted_sequence);

CREATE UNIQUE INDEX repository_import_active_session
ON repository_import_requests (session_id)
WHERE state IN (0, 1, 2, 4);

CREATE TABLE repository_import_facts (
    fact_id BLOB PRIMARY KEY NOT NULL CHECK (length(fact_id) = 16),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    request_id BLOB NOT NULL REFERENCES repository_import_requests(request_id),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    workspace_id BLOB NOT NULL CHECK (length(workspace_id) = 16),
    operation_id BLOB NOT NULL CHECK (length(operation_id) = 16),
    fact_kind INTEGER NOT NULL CHECK (fact_kind BETWEEN 1 AND 5),
    file_count INTEGER CHECK (file_count IS NULL OR file_count >= 0),
    directory_count INTEGER CHECK (directory_count IS NULL OR directory_count >= 0),
    logical_bytes INTEGER CHECK (logical_bytes IS NULL OR logical_bytes >= 0),
    manifest_digest BLOB CHECK (manifest_digest IS NULL OR length(manifest_digest) = 32),
    delivery_event_id BLOB UNIQUE CHECK (
        delivery_event_id IS NULL OR length(delivery_event_id) = 16
    ),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    CHECK (
        (fact_kind = 3 AND file_count IS NOT NULL AND directory_count IS NOT NULL
                       AND logical_bytes IS NOT NULL AND manifest_digest IS NOT NULL)
        OR
        (fact_kind != 3 AND file_count IS NULL AND directory_count IS NULL
                        AND logical_bytes IS NULL AND manifest_digest IS NULL)
    ),
    CHECK ((fact_kind = 2) = (delivery_event_id IS NULL)),
    UNIQUE (request_id, fact_kind)
) STRICT, WITHOUT ROWID;

CREATE INDEX repository_import_facts_by_session
ON repository_import_facts (session_id, fact_sequence);

CREATE TABLE repository_import_audit_facts (
    audit_id BLOB PRIMARY KEY NOT NULL CHECK (length(audit_id) = 16),
    audit_sequence INTEGER NOT NULL UNIQUE CHECK (audit_sequence > 0),
    request_id BLOB NOT NULL REFERENCES repository_import_requests(request_id),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    operation_id BLOB NOT NULL CHECK (length(operation_id) = 16),
    audit_kind INTEGER NOT NULL CHECK (audit_kind BETWEEN 1 AND 5),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    UNIQUE (request_id, audit_kind)
) STRICT, WITHOUT ROWID;

PRAGMA user_version = 5;

COMMIT;
