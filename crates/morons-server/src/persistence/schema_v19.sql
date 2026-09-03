BEGIN IMMEDIATE;

CREATE TABLE context_checkpoints (
    checkpoint_id BLOB PRIMARY KEY NOT NULL CHECK (length(checkpoint_id) = 16),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    parent_checkpoint_id BLOB REFERENCES context_checkpoints(checkpoint_id),
    source_entry_high_water INTEGER NOT NULL CHECK (source_entry_high_water > 0),
    source_digest BLOB NOT NULL CHECK (length(source_digest) = 32),
    context_policy_version INTEGER NOT NULL CHECK (context_policy_version = 4),
    open_code_service INTEGER NOT NULL CHECK (open_code_service IN (1, 2)),
    model_id TEXT NOT NULL CHECK (length(CAST(model_id AS BLOB)) BETWEEN 1 AND 128),
    summary TEXT NOT NULL CHECK (length(CAST(summary AS BLOB)) BETWEEN 1 AND 131072),
    estimated_summary_tokens INTEGER NOT NULL CHECK (estimated_summary_tokens BETWEEN 1 AND 96000),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    UNIQUE (session_id, source_entry_high_water)
) STRICT, WITHOUT ROWID;
CREATE INDEX context_checkpoints_by_session
ON context_checkpoints (session_id, source_entry_high_water);

CREATE TABLE run_accepted_checkpoints (
    run_id BLOB PRIMARY KEY NOT NULL REFERENCES run_accepted_facts(run_id),
    checkpoint_id BLOB NOT NULL REFERENCES context_checkpoints(checkpoint_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE compaction_operations (
    operation_id BLOB PRIMARY KEY NOT NULL CHECK (length(operation_id) = 16),
    run_id BLOB NOT NULL UNIQUE REFERENCES run_accepted_facts(run_id),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    parent_checkpoint_id BLOB REFERENCES context_checkpoints(checkpoint_id),
    source_entry_high_water INTEGER NOT NULL CHECK (source_entry_high_water > 0),
    source_digest BLOB NOT NULL CHECK (length(source_digest) = 32),
    state INTEGER NOT NULL CHECK (state BETWEEN 1 AND 5),
    checkpoint_id BLOB UNIQUE REFERENCES context_checkpoints(checkpoint_id),
    prepared_sequence INTEGER NOT NULL UNIQUE CHECK (prepared_sequence > 0),
    updated_sequence INTEGER NOT NULL UNIQUE CHECK (updated_sequence > 0),
    prepared_at_milliseconds INTEGER NOT NULL CHECK (prepared_at_milliseconds >= 0),
    updated_at_milliseconds INTEGER NOT NULL CHECK (updated_at_milliseconds >= prepared_at_milliseconds),
    CHECK ((state = 3 AND checkpoint_id IS NOT NULL) OR (state != 3 AND checkpoint_id IS NULL))
) STRICT, WITHOUT ROWID;

PRAGMA user_version = 19;

COMMIT;
