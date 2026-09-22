BEGIN IMMEDIATE;

DROP INDEX session_entries_user_by_run;
CREATE INDEX session_entries_user_by_run ON session_entries (run_id) WHERE entry_kind = 1;

CREATE TABLE steering_delivery_facts (
    fact_sequence INTEGER PRIMARY KEY CHECK (fact_sequence > 0),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    run_id BLOB NOT NULL,
    item_id BLOB NOT NULL UNIQUE CHECK (length(item_id) = 16),
    item_revision INTEGER NOT NULL CHECK (item_revision > 0),
    enqueue_sequence INTEGER NOT NULL CHECK (enqueue_sequence > 0),
    queue_revision INTEGER NOT NULL CHECK (queue_revision > 1),
    message_id BLOB NOT NULL UNIQUE REFERENCES session_entries(message_id),
    source_entry_high_water INTEGER NOT NULL CHECK (source_entry_high_water > 0),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    UNIQUE (session_id, queue_revision),
    FOREIGN KEY (session_id, run_id) REFERENCES run_accepted_facts(session_id, run_id)
) STRICT;

PRAGMA user_version = 45;
COMMIT;
