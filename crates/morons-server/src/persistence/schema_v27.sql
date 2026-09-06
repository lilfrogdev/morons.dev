BEGIN IMMEDIATE;

CREATE TABLE compaction_maintenance_jobs (
    job_id BLOB PRIMARY KEY NOT NULL CHECK (length(job_id) = 16),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    trigger_run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id),
    parent_checkpoint_id BLOB REFERENCES context_checkpoints(checkpoint_id),
    source_entry_high_water INTEGER NOT NULL CHECK (source_entry_high_water > 0),
    prepared_entry_high_water INTEGER NOT NULL CHECK (prepared_entry_high_water > source_entry_high_water),
    source_digest BLOB NOT NULL CHECK (length(source_digest) = 32),
    instruction_digest BLOB NOT NULL CHECK (length(instruction_digest) = 32),
    binding_digest BLOB NOT NULL CHECK (length(binding_digest) = 32),
    maintenance_policy_version INTEGER NOT NULL CHECK (maintenance_policy_version = 1),
    state INTEGER NOT NULL CHECK (state BETWEEN 1 AND 8),
    prepared_sequence INTEGER NOT NULL UNIQUE CHECK (prepared_sequence > 0),
    prepared_at_milliseconds INTEGER NOT NULL CHECK (prepared_at_milliseconds >= 0),
    UNIQUE (session_id, source_entry_high_water)
) STRICT, WITHOUT ROWID;
CREATE INDEX compaction_maintenance_by_session
ON compaction_maintenance_jobs (session_id, prepared_sequence);
CREATE UNIQUE INDEX compaction_maintenance_one_pending_session
ON compaction_maintenance_jobs (session_id) WHERE state IN (1, 2, 3);
CREATE UNIQUE INDEX compaction_maintenance_one_dispatched
ON compaction_maintenance_jobs (state) WHERE state = 2;

CREATE TABLE compaction_maintenance_events (
    job_id BLOB NOT NULL REFERENCES compaction_maintenance_jobs(job_id),
    state INTEGER NOT NULL CHECK (state BETWEEN 2 AND 8),
    fact_sequence INTEGER PRIMARY KEY NOT NULL CHECK (fact_sequence > 0),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    result_payload TEXT CHECK (result_payload IS NULL OR length(CAST(result_payload AS BLOB)) BETWEEN 2 AND 102400),
    result_digest BLOB CHECK (result_digest IS NULL OR length(result_digest) = 32),
    checkpoint_id BLOB REFERENCES context_checkpoints(checkpoint_id),
    installed_run_id BLOB REFERENCES run_accepted_facts(run_id),
    installed_entry_high_water INTEGER CHECK (installed_entry_high_water IS NULL OR installed_entry_high_water > 0),
    UNIQUE (job_id, state),
    CHECK ((state = 3 AND result_payload IS NOT NULL AND result_digest IS NOT NULL)
        OR (state != 3 AND result_payload IS NULL AND result_digest IS NULL)),
    CHECK ((state = 8 AND checkpoint_id IS NOT NULL AND installed_run_id IS NOT NULL AND installed_entry_high_water IS NOT NULL)
        OR (state != 8 AND checkpoint_id IS NULL AND installed_run_id IS NULL AND installed_entry_high_water IS NULL))
) STRICT;
CREATE INDEX compaction_maintenance_events_by_job
ON compaction_maintenance_events (job_id, fact_sequence);
CREATE UNIQUE INDEX compaction_maintenance_installed_checkpoint
ON compaction_maintenance_events (checkpoint_id) WHERE checkpoint_id IS NOT NULL;
CREATE INDEX compaction_operations_by_prefix
ON compaction_operations (session_id, source_entry_high_water);

PRAGMA user_version = 27;

COMMIT;
