BEGIN IMMEDIATE;

CREATE TABLE run_skill_snapshots (
    run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id),
    skill_index INTEGER NOT NULL CHECK (skill_index BETWEEN 1 AND 128),
    skill_name TEXT NOT NULL CHECK (length(CAST(skill_name AS BLOB)) BETWEEN 1 AND 64),
    description TEXT NOT NULL CHECK (length(CAST(description AS BLOB)) BETWEEN 1 AND 1024),
    skill_file TEXT NOT NULL CHECK (length(CAST(skill_file AS BLOB)) BETWEEN 1 AND 4096),
    skill_source INTEGER NOT NULL CHECK (skill_source BETWEEN 1 AND 3),
    active INTEGER NOT NULL CHECK (active IN (0, 1)),
    instructions TEXT CHECK (instructions IS NULL OR length(CAST(instructions AS BLOB)) BETWEEN 1 AND 131072),
    PRIMARY KEY (run_id, skill_index),
    UNIQUE (run_id, skill_name),
    CHECK ((active = 1 AND instructions IS NOT NULL) OR (active = 0 AND instructions IS NULL))
) STRICT, WITHOUT ROWID;

PRAGMA user_version = 17;

COMMIT;
