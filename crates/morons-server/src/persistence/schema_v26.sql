CREATE TABLE run_project_contexts (
    run_id BLOB PRIMARY KEY NOT NULL REFERENCES run_accepted_facts(run_id)
        CHECK (length(run_id) = 16),
    snapshot TEXT NOT NULL CHECK (length(CAST(snapshot AS BLOB)) <= 65536),
    source_digest BLOB NOT NULL CHECK (length(source_digest) = 32)
) STRICT;

PRAGMA user_version = 26;
