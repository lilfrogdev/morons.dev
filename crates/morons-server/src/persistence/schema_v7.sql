BEGIN IMMEDIATE;

PRAGMA defer_foreign_keys = ON;

CREATE TABLE mutation_requests_v7 (
    request_id BLOB PRIMARY KEY NOT NULL CHECK (length(request_id) = 16),
    operation_kind INTEGER NOT NULL CHECK (operation_kind BETWEEN 1 AND 9),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;
INSERT INTO mutation_requests_v7 SELECT * FROM mutation_requests;
DROP TABLE mutation_requests;
ALTER TABLE mutation_requests_v7 RENAME TO mutation_requests;

CREATE TABLE execution_image_requests (
    request_id BLOB PRIMARY KEY NOT NULL REFERENCES mutation_requests(request_id),
    operation_fingerprint BLOB NOT NULL CHECK (length(operation_fingerprint) = 32),
    toolchain_source_digest BLOB NOT NULL CHECK (length(toolchain_source_digest) = 32),
    cargo_source_digest BLOB NOT NULL CHECK (length(cargo_source_digest) = 32),
    generation_id BLOB NOT NULL UNIQUE CHECK (length(generation_id) = 16),
    operation_id BLOB NOT NULL UNIQUE CHECK (length(operation_id) = 16),
    target_os INTEGER NOT NULL CHECK (target_os BETWEEN 1 AND 3),
    target_arch INTEGER NOT NULL CHECK (target_arch BETWEEN 1 AND 2),
    format_version INTEGER NOT NULL CHECK (format_version = 1),
    limits_version INTEGER NOT NULL CHECK (limits_version = 1),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0),
    state INTEGER NOT NULL CHECK (state BETWEEN 0 AND 4),
    file_count INTEGER CHECK (file_count IS NULL OR file_count BETWEEN 2 AND 200000),
    directory_count INTEGER CHECK (directory_count IS NULL OR directory_count BETWEEN 2 AND 200000),
    logical_bytes INTEGER CHECK (logical_bytes IS NULL OR logical_bytes BETWEEN 1 AND 8589934592),
    manifest_digest BLOB CHECK (manifest_digest IS NULL OR length(manifest_digest) = 32),
    CHECK (
        (state = 2 AND file_count IS NOT NULL AND directory_count IS NOT NULL
         AND logical_bytes IS NOT NULL AND manifest_digest IS NOT NULL)
        OR
        (state != 2 AND file_count IS NULL AND directory_count IS NULL
         AND logical_bytes IS NULL AND manifest_digest IS NULL)
    )
) STRICT, WITHOUT ROWID;
CREATE INDEX execution_image_requests_by_state
ON execution_image_requests (state, accepted_sequence);
CREATE UNIQUE INDEX execution_image_single_incomplete
ON execution_image_requests ((1)) WHERE state IN (0, 1);

CREATE TABLE execution_image_facts (
    fact_id BLOB PRIMARY KEY NOT NULL CHECK (length(fact_id) = 16),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    request_id BLOB NOT NULL REFERENCES execution_image_requests(request_id),
    generation_id BLOB NOT NULL CHECK (length(generation_id) = 16),
    operation_id BLOB NOT NULL CHECK (length(operation_id) = 16),
    fact_kind INTEGER NOT NULL CHECK (fact_kind BETWEEN 1 AND 5),
    file_count INTEGER CHECK (file_count IS NULL OR file_count BETWEEN 2 AND 200000),
    directory_count INTEGER CHECK (directory_count IS NULL OR directory_count BETWEEN 2 AND 200000),
    logical_bytes INTEGER CHECK (logical_bytes IS NULL OR logical_bytes BETWEEN 1 AND 8589934592),
    manifest_digest BLOB CHECK (manifest_digest IS NULL OR length(manifest_digest) = 32),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    UNIQUE (request_id, fact_kind),
    CHECK (
        (fact_kind = 3 AND file_count IS NOT NULL AND directory_count IS NOT NULL
         AND logical_bytes IS NOT NULL AND manifest_digest IS NOT NULL)
        OR
        (fact_kind != 3 AND file_count IS NULL AND directory_count IS NULL
         AND logical_bytes IS NULL AND manifest_digest IS NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE TABLE execution_image_audit_facts (
    audit_id BLOB PRIMARY KEY NOT NULL CHECK (length(audit_id) = 16),
    audit_sequence INTEGER NOT NULL UNIQUE CHECK (audit_sequence > 0),
    request_id BLOB NOT NULL REFERENCES execution_image_requests(request_id),
    generation_id BLOB NOT NULL CHECK (length(generation_id) = 16),
    operation_id BLOB NOT NULL CHECK (length(operation_id) = 16),
    audit_kind INTEGER NOT NULL CHECK (audit_kind BETWEEN 1 AND 5),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    UNIQUE (request_id, audit_kind)
) STRICT, WITHOUT ROWID;

CREATE TABLE current_execution_image (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    request_id BLOB NOT NULL UNIQUE REFERENCES execution_image_requests(request_id),
    generation_id BLOB NOT NULL UNIQUE CHECK (length(generation_id) = 16),
    updated_sequence INTEGER NOT NULL CHECK (updated_sequence > 0)
) STRICT;

PRAGMA user_version = 7;

COMMIT;
