BEGIN IMMEDIATE;
CREATE TABLE web_search_attempts (
    call_id BLOB NOT NULL REFERENCES web_model_bindings(call_id) ON DELETE CASCADE,
    child INTEGER NOT NULL CHECK (child BETWEEN 0 AND 3),
    ordinal INTEGER NOT NULL CHECK (ordinal BETWEEN 0 AND 24),
    query_digest BLOB NOT NULL CHECK (length(query_digest) = 32),
    admission_digest BLOB NOT NULL CHECK (length(admission_digest) = 32),
    route INTEGER NOT NULL CHECK (route IN (0, 1, 2)),
    contract_revision INTEGER NOT NULL CHECK (contract_revision = 1),
    policy_sequence INTEGER NOT NULL CHECK (policy_sequence >= 0),
    dispatch_sequence INTEGER NOT NULL UNIQUE CHECK (dispatch_sequence > policy_sequence),
    PRIMARY KEY (call_id, child, ordinal),
    CHECK ((child = 0 AND ordinal = 0) OR (child > 0 AND ordinal > 0))
) STRICT, WITHOUT ROWID;
PRAGMA user_version = 35;
COMMIT;
