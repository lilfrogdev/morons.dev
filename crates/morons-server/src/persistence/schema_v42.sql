BEGIN IMMEDIATE;
CREATE TABLE child_runs (
    call_id BLOB NOT NULL REFERENCES task_model_bindings(call_id),
    child INTEGER NOT NULL CHECK(child BETWEEN 1 AND 3),
    state INTEGER NOT NULL CHECK(state IN (1,2,3)),
    PRIMARY KEY(call_id,child)
) STRICT;
CREATE TABLE child_journal (
    call_id BLOB NOT NULL,
    child INTEGER NOT NULL,
    ordinal INTEGER NOT NULL CHECK(ordinal > 0),
    kind INTEGER NOT NULL CHECK(kind BETWEEN 1 AND 9),
    payload BLOB NOT NULL CHECK(length(payload) BETWEEN 1 AND 16777216),
    previous_digest BLOB NOT NULL CHECK(length(previous_digest)=32),
    digest BLOB NOT NULL CHECK(length(digest)=32),
    PRIMARY KEY(call_id,child,ordinal),
    FOREIGN KEY(call_id,child) REFERENCES child_runs(call_id,child)
) STRICT;
PRAGMA user_version = 42;
COMMIT;
