BEGIN IMMEDIATE;
PRAGMA defer_foreign_keys = ON;

CREATE TABLE credential_mutation_requests_v28 (
    request_id BLOB PRIMARY KEY NOT NULL REFERENCES mutation_requests(request_id),
    operation_kind INTEGER NOT NULL CHECK (operation_kind IN (2, 3)),
    expected_generation INTEGER NOT NULL CHECK (expected_generation >= 0),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0),
    state INTEGER NOT NULL CHECK (state IN (0, 1, 2, 3)),
    result_generation INTEGER CHECK (result_generation IS NULL OR result_generation > 0),
    result_configured INTEGER CHECK (result_configured IS NULL OR result_configured IN (0, 1)),
    credential_kind INTEGER NOT NULL DEFAULT 1 CHECK (credential_kind IN (1, 2)),
    CHECK ((state = 2 AND result_generation IS NOT NULL AND result_configured IS NOT NULL)
        OR (state != 2 AND result_generation IS NULL AND result_configured IS NULL)),
    UNIQUE (credential_kind, result_generation)
) STRICT, WITHOUT ROWID;
INSERT INTO credential_mutation_requests_v28
    SELECT request_id, operation_kind, expected_generation, accepted_sequence,
        accepted_at_milliseconds, state, result_generation, result_configured, 1
    FROM credential_mutation_requests;
DROP TABLE credential_mutation_requests;
ALTER TABLE credential_mutation_requests_v28 RENAME TO credential_mutation_requests;
CREATE INDEX credential_mutation_requests_by_state ON credential_mutation_requests (state, accepted_sequence);
CREATE INDEX credential_mutation_requests_by_provider ON credential_mutation_requests (credential_kind, state, result_generation);

PRAGMA user_version = 28;
COMMIT;
