BEGIN IMMEDIATE;
CREATE TABLE web_attempt_epoch (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    first_sequence INTEGER NOT NULL CHECK (first_sequence > 0)
) STRICT;
INSERT INTO web_attempt_epoch SELECT singleton, next_value FROM logical_sequences;
PRAGMA user_version = 36;
COMMIT;
