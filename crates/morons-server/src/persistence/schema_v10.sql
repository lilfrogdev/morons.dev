BEGIN IMMEDIATE;
CREATE TABLE mutation_requests_v10 (
 request_id BLOB PRIMARY KEY NOT NULL CHECK(length(request_id)=16),
 operation_kind INTEGER NOT NULL CHECK(operation_kind BETWEEN 1 AND 10),
 accepted_sequence INTEGER NOT NULL UNIQUE CHECK(accepted_sequence>0),
 accepted_at_milliseconds INTEGER NOT NULL CHECK(accepted_at_milliseconds>=0)
) STRICT, WITHOUT ROWID;
INSERT INTO mutation_requests_v10 SELECT * FROM mutation_requests;
DROP TABLE mutation_requests;
ALTER TABLE mutation_requests_v10 RENAME TO mutation_requests;
CREATE TABLE export_requests (
 request_id BLOB PRIMARY KEY NOT NULL REFERENCES mutation_requests(request_id),
 fingerprint BLOB NOT NULL CHECK(length(fingerprint)=32),
 destination_digest BLOB NOT NULL CHECK(length(destination_digest)=32),
 session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
 workspace_id BLOB NOT NULL CHECK(length(workspace_id)=16),
 generation_id BLOB NOT NULL CHECK(length(generation_id)=16),
 operation_id BLOB NOT NULL UNIQUE CHECK(length(operation_id)=16),
 state INTEGER NOT NULL CHECK(state BETWEEN 0 AND 4),
 accepted_sequence INTEGER NOT NULL UNIQUE CHECK(accepted_sequence>0),
 accepted_at_milliseconds INTEGER NOT NULL,
 file_count INTEGER, directory_count INTEGER, logical_bytes INTEGER,
 CHECK((state=2 AND file_count IS NOT NULL AND directory_count IS NOT NULL AND logical_bytes IS NOT NULL)
    OR (state!=2 AND file_count IS NULL AND directory_count IS NULL AND logical_bytes IS NULL))
) STRICT, WITHOUT ROWID;
CREATE INDEX export_requests_by_state ON export_requests(state,accepted_sequence);
PRAGMA user_version=10;
COMMIT;
