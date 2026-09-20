BEGIN IMMEDIATE;

CREATE UNIQUE INDEX run_accepted_facts_by_session_run
ON run_accepted_facts (session_id, run_id);

CREATE TABLE steering_queues (
    session_id BLOB PRIMARY KEY NOT NULL REFERENCES session_created_facts(session_id),
    target_run_id BLOB NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    paused INTEGER NOT NULL DEFAULT 1 CHECK (paused IN (0, 1)),
    FOREIGN KEY (session_id, target_run_id)
        REFERENCES run_accepted_facts(session_id, run_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE steering_pending_messages (
    item_id BLOB PRIMARY KEY NOT NULL CHECK (length(item_id) = 16),
    session_id BLOB NOT NULL REFERENCES steering_queues(session_id),
    slot INTEGER NOT NULL CHECK (slot BETWEEN 1 AND 16),
    enqueue_sequence INTEGER NOT NULL CHECK (enqueue_sequence > 0),
    revision INTEGER NOT NULL CHECK (revision > 0),
    text TEXT NOT NULL CHECK (length(CAST(text AS BLOB)) BETWEEN 1 AND 65536),
    actor INTEGER NOT NULL CHECK (actor = 1),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    UNIQUE (session_id, slot),
    UNIQUE (session_id, enqueue_sequence)
) STRICT, WITHOUT ROWID;

PRAGMA user_version = 43;
COMMIT;
