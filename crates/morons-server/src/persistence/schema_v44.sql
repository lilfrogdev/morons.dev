BEGIN IMMEDIATE;
PRAGMA defer_foreign_keys = ON;

CREATE TABLE mutation_requests_v44 (
    request_id BLOB PRIMARY KEY NOT NULL CHECK (length(request_id) = 16),
    operation_kind INTEGER NOT NULL CHECK (operation_kind BETWEEN 1 AND 18),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;
INSERT INTO mutation_requests_v44 SELECT * FROM mutation_requests;
DROP TABLE mutation_requests;
ALTER TABLE mutation_requests_v44 RENAME TO mutation_requests;

CREATE TABLE deleted_mutation_tombstones_v44 (
    request_id BLOB PRIMARY KEY NOT NULL CHECK (length(request_id) = 16),
    delete_request_id BLOB NOT NULL REFERENCES session_delete_requests(request_id),
    operation_kind INTEGER NOT NULL CHECK (operation_kind BETWEEN 1 AND 18),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;
INSERT INTO deleted_mutation_tombstones_v44 SELECT * FROM deleted_mutation_tombstones;
DROP TABLE deleted_mutation_tombstones;
ALTER TABLE deleted_mutation_tombstones_v44 RENAME TO deleted_mutation_tombstones;

CREATE TABLE steering_mutation_requests (
    request_id BLOB PRIMARY KEY NOT NULL REFERENCES mutation_requests(request_id),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    operation_fingerprint BLOB NOT NULL CHECK (length(operation_fingerprint) = 32),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0),
    queue_revision INTEGER NOT NULL CHECK (queue_revision > 0),
    item_id BLOB CHECK (item_id IS NULL OR length(item_id) = 16),
    item_revision INTEGER CHECK (item_revision IS NULL OR item_revision > 0),
    actor INTEGER NOT NULL CHECK (actor = 1),
    change_kind INTEGER NOT NULL CHECK (change_kind BETWEEN 1 AND 5),
    target_run_id BLOB,
    text TEXT CHECK (text IS NULL OR length(CAST(text AS BLOB)) BETWEEN 1 AND 65536),
    UNIQUE (session_id, queue_revision),
    FOREIGN KEY (session_id, target_run_id) REFERENCES run_accepted_facts(session_id, run_id),
    CHECK (
        (change_kind = 1 AND target_run_id IS NOT NULL AND text IS NOT NULL
            AND item_id IS NOT NULL AND item_revision IS 1)
        OR (change_kind = 2 AND target_run_id IS NULL AND text IS NOT NULL
            AND item_id IS NOT NULL AND item_revision IS NOT NULL AND item_revision > 1)
        OR (change_kind = 3 AND target_run_id IS NULL AND text IS NULL
            AND item_id IS NOT NULL AND item_revision IS NOT NULL)
        OR (change_kind = 4 AND target_run_id IS NULL AND text IS NULL
            AND item_id IS NULL AND item_revision IS NULL)
        OR (change_kind = 5 AND target_run_id IS NOT NULL AND text IS NULL
            AND item_id IS NULL AND item_revision IS NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE TABLE steering_lifecycle_facts (
    fact_sequence INTEGER PRIMARY KEY CHECK (fact_sequence > 0),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    target_run_id BLOB NOT NULL,
    queue_revision INTEGER NOT NULL CHECK (queue_revision > 1),
    reason INTEGER NOT NULL CHECK (reason BETWEEN 1 AND 4),
    source_sequence INTEGER CHECK (source_sequence > 0),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    UNIQUE (session_id, queue_revision),
    FOREIGN KEY (session_id, target_run_id) REFERENCES run_accepted_facts(session_id, run_id),
    CHECK ((reason = 4 AND source_sequence IS NULL)
        OR (reason != 4 AND source_sequence IS NOT NULL))
) STRICT;

PRAGMA user_version = 44;
COMMIT;
