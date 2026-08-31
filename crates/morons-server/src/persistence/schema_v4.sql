BEGIN IMMEDIATE;

PRAGMA defer_foreign_keys = ON;

CREATE TABLE mutation_requests_v4 (
    request_id BLOB PRIMARY KEY NOT NULL CHECK (length(request_id) = 16),
    operation_kind INTEGER NOT NULL CHECK (operation_kind IN (1, 2, 3, 4, 5, 6)),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;

INSERT INTO mutation_requests_v4 (
    request_id,
    operation_kind,
    accepted_sequence,
    accepted_at_milliseconds
)
SELECT request_id, operation_kind, accepted_sequence, accepted_at_milliseconds
FROM mutation_requests;

DROP TABLE mutation_requests;
ALTER TABLE mutation_requests_v4 RENAME TO mutation_requests;

CREATE TABLE server_stop_requests (
    request_id BLOB PRIMARY KEY NOT NULL REFERENCES mutation_requests(request_id),
    operation_fingerprint BLOB NOT NULL CHECK (length(operation_fingerprint) = 32),
    host_epoch BLOB NOT NULL CHECK (length(host_epoch) = 16),
    signal_applied INTEGER NOT NULL CHECK (signal_applied IN (0, 1)),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX server_stop_signal_by_host_epoch
ON server_stop_requests (host_epoch)
WHERE signal_applied = 1;

CREATE TABLE server_audit_facts (
    audit_id BLOB PRIMARY KEY NOT NULL CHECK (length(audit_id) = 16),
    audit_sequence INTEGER NOT NULL UNIQUE CHECK (audit_sequence > 0),
    request_id BLOB NOT NULL REFERENCES server_stop_requests(request_id),
    host_epoch BLOB NOT NULL CHECK (length(host_epoch) = 16),
    audit_kind INTEGER NOT NULL CHECK (audit_kind = 1),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    UNIQUE (request_id, audit_kind)
) STRICT, WITHOUT ROWID;

PRAGMA user_version = 4;

COMMIT;
