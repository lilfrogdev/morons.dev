BEGIN IMMEDIATE;
CREATE TABLE context_accounting_epoch (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    first_sequence INTEGER NOT NULL CHECK (first_sequence > 0)
) STRICT;
INSERT INTO context_accounting_epoch SELECT singleton, next_value FROM logical_sequences;

CREATE TABLE compaction_operations_v33 (
    operation_id BLOB PRIMARY KEY NOT NULL CHECK (length(operation_id) = 16),
    run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id),
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
    CHECK ((state = 3 AND checkpoint_id IS NOT NULL) OR (state != 3 AND checkpoint_id IS NULL)),
    UNIQUE (run_id, source_entry_high_water)
) STRICT, WITHOUT ROWID;
INSERT INTO compaction_operations_v33 SELECT * FROM compaction_operations;
DROP TABLE compaction_operations;
ALTER TABLE compaction_operations_v33 RENAME TO compaction_operations;
CREATE INDEX compaction_operations_by_prefix ON compaction_operations(session_id, source_entry_high_water);
PRAGMA user_version = 33;
COMMIT;
