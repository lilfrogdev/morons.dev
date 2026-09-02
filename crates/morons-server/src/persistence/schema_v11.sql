BEGIN IMMEDIATE;

ALTER TABLE session_creation_requests
ADD COLUMN working_directory TEXT
CHECK (working_directory IS NULL OR length(working_directory) BETWEEN 1 AND 4096);

ALTER TABLE session_created_facts
ADD COLUMN working_directory TEXT
CHECK (working_directory IS NULL OR length(working_directory) BETWEEN 1 AND 4096);

ALTER TABLE sessions
ADD COLUMN working_directory TEXT
CHECK (working_directory IS NULL OR length(working_directory) BETWEEN 1 AND 4096);

PRAGMA user_version = 11;

COMMIT;
