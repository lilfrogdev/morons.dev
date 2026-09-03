BEGIN IMMEDIATE;

PRAGMA defer_foreign_keys = ON;

CREATE TABLE tool_calls_v16 (
    call_id BLOB PRIMARY KEY NOT NULL CHECK (length(call_id) = 16),
    operation_id BLOB NOT NULL UNIQUE CHECK (length(operation_id) = 16),
    provider_operation_id BLOB NOT NULL CHECK (length(provider_operation_id) = 16),
    provider_call_id TEXT NOT NULL CHECK (length(CAST(provider_call_id AS BLOB)) BETWEEN 1 AND 128),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id),
    call_index INTEGER NOT NULL CHECK (call_index BETWEEN 1 AND 8),
    tool_kind INTEGER NOT NULL CHECK (tool_kind BETWEEN 1 AND 13),
    input_version INTEGER NOT NULL CHECK (input_version = 1),
    input_payload BLOB NOT NULL CHECK (length(input_payload) BETWEEN 2 AND 524288),
    path_digest BLOB NOT NULL CHECK (length(path_digest) = 32),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    UNIQUE (run_id, provider_call_id),
    UNIQUE (provider_operation_id, call_index)
) STRICT, WITHOUT ROWID;
INSERT INTO tool_calls_v16 SELECT * FROM tool_calls;
DROP TABLE tool_calls;
ALTER TABLE tool_calls_v16 RENAME TO tool_calls;
CREATE INDEX tool_calls_by_run ON tool_calls (run_id, fact_sequence);

CREATE TABLE tool_audit_facts_v16 (
    audit_id BLOB PRIMARY KEY NOT NULL CHECK (length(audit_id) = 16),
    audit_sequence INTEGER NOT NULL UNIQUE CHECK (audit_sequence > 0),
    call_id BLOB REFERENCES tool_calls(call_id),
    request_id BLOB REFERENCES tool_uncertainty_acknowledgements(request_id),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id),
    operation_id BLOB CHECK (operation_id IS NULL OR length(operation_id) = 16),
    tool_kind INTEGER CHECK (tool_kind IS NULL OR tool_kind BETWEEN 1 AND 13),
    audit_kind INTEGER NOT NULL CHECK (audit_kind BETWEEN 1 AND 6),
    path_digest BLOB CHECK (path_digest IS NULL OR length(path_digest) = 32),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    CHECK (
        (audit_kind BETWEEN 1 AND 5 AND call_id IS NOT NULL AND request_id IS NULL
         AND operation_id IS NOT NULL AND tool_kind IS NOT NULL AND path_digest IS NOT NULL)
        OR
        (audit_kind = 6 AND call_id IS NULL AND request_id IS NOT NULL
         AND operation_id IS NULL AND tool_kind IS NULL AND path_digest IS NULL)
    )
) STRICT, WITHOUT ROWID;
INSERT INTO tool_audit_facts_v16 SELECT * FROM tool_audit_facts;
DROP TABLE tool_audit_facts;
ALTER TABLE tool_audit_facts_v16 RENAME TO tool_audit_facts;
CREATE INDEX tool_audit_facts_by_run ON tool_audit_facts (run_id, audit_sequence);

PRAGMA user_version = 16;

COMMIT;
