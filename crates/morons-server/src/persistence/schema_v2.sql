BEGIN IMMEDIATE;

CREATE TABLE mutation_requests (
    request_id BLOB PRIMARY KEY NOT NULL CHECK (length(request_id) = 16),
    operation_kind INTEGER NOT NULL CHECK (operation_kind IN (1, 2, 3)),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;

INSERT INTO mutation_requests (
    request_id,
    operation_kind,
    accepted_sequence,
    accepted_at_milliseconds
)
SELECT
    request_id,
    1,
    accepted_sequence,
    accepted_at_milliseconds
FROM session_creation_requests;

CREATE TABLE credential_mutation_requests (
    request_id BLOB PRIMARY KEY NOT NULL REFERENCES mutation_requests(request_id),
    operation_kind INTEGER NOT NULL CHECK (operation_kind IN (2, 3)),
    expected_generation INTEGER NOT NULL CHECK (expected_generation >= 0),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0),
    state INTEGER NOT NULL CHECK (state IN (0, 1, 2, 3)),
    result_generation INTEGER UNIQUE CHECK (result_generation IS NULL OR result_generation > 0),
    result_configured INTEGER CHECK (result_configured IS NULL OR result_configured IN (0, 1)),
    CHECK (
        (state = 2 AND result_generation IS NOT NULL AND result_configured IS NOT NULL)
        OR (state != 2 AND result_generation IS NULL AND result_configured IS NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE INDEX credential_mutation_requests_by_state
ON credential_mutation_requests (state, accepted_sequence);

CREATE TABLE credential_operation_facts (
    fact_id BLOB PRIMARY KEY NOT NULL CHECK (length(fact_id) = 16),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    request_id BLOB NOT NULL REFERENCES credential_mutation_requests(request_id),
    operation_kind INTEGER NOT NULL CHECK (operation_kind IN (1, 2, 3)),
    credential_generation INTEGER NOT NULL CHECK (credential_generation > 0),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    UNIQUE (request_id, operation_kind)
) STRICT, WITHOUT ROWID;

CREATE TABLE credential_audit_facts (
    audit_id BLOB PRIMARY KEY NOT NULL CHECK (length(audit_id) = 16),
    audit_sequence INTEGER NOT NULL UNIQUE CHECK (audit_sequence > 0),
    request_id BLOB NOT NULL REFERENCES credential_mutation_requests(request_id),
    actor_kind INTEGER NOT NULL CHECK (actor_kind = 1),
    audit_kind INTEGER NOT NULL CHECK (audit_kind IN (1, 2, 3, 4)),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    UNIQUE (request_id, audit_kind)
) STRICT, WITHOUT ROWID;

PRAGMA user_version = 2;

COMMIT;
