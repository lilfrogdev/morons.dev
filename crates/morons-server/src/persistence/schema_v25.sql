BEGIN IMMEDIATE;

PRAGMA defer_foreign_keys = ON;

CREATE TABLE mutation_requests_v25 (
    request_id BLOB PRIMARY KEY NOT NULL CHECK (length(request_id) = 16),
    operation_kind INTEGER NOT NULL CHECK (operation_kind BETWEEN 1 AND 16),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;
INSERT INTO mutation_requests_v25 SELECT * FROM mutation_requests;
DROP TABLE mutation_requests;
ALTER TABLE mutation_requests_v25 RENAME TO mutation_requests;

CREATE TABLE subagent_model_selections (
    request_id BLOB PRIMARY KEY NOT NULL REFERENCES mutation_requests(request_id),
    operation_fingerprint BLOB NOT NULL CHECK (length(operation_fingerprint) = 32),
    selection_kind INTEGER NOT NULL CHECK (selection_kind IN (1, 2)),
    open_code_service INTEGER CHECK (open_code_service IN (1, 2)),
    model_id TEXT CHECK (
        model_id IS NULL
        OR length(CAST(model_id AS BLOB)) BETWEEN 1 AND 128
    ),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0),
    CHECK (
        (selection_kind = 1 AND open_code_service IS NULL AND model_id IS NULL)
        OR
        (selection_kind = 2 AND open_code_service IS NOT NULL AND model_id IS NOT NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE INDEX subagent_model_selections_by_sequence
ON subagent_model_selections (accepted_sequence);

PRAGMA user_version = 25;

COMMIT;
