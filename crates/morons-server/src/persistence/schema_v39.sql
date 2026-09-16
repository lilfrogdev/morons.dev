BEGIN IMMEDIATE;
CREATE TABLE web_provenance_epoch (
    singleton INTEGER PRIMARY KEY CHECK (singleton=1),
    first_sequence INTEGER NOT NULL CHECK (first_sequence>0)
) STRICT;
INSERT INTO web_provenance_epoch SELECT 1,next_value FROM logical_sequences WHERE singleton=1;
CREATE TABLE web_binding_provenance (
    call_id BLOB PRIMARY KEY REFERENCES web_model_bindings(call_id) CHECK(length(call_id)=16),
    absence_generation INTEGER CHECK(absence_generation>=0),
    evidence_digest BLOB NOT NULL CHECK(length(evidence_digest)=32)
) STRICT;
ALTER TABLE web_model_bindings ADD COLUMN success_count INTEGER NOT NULL DEFAULT 0 CHECK(success_count BETWEEN 0 AND 72);
CREATE TABLE web_search_successes (
    call_id BLOB NOT NULL,
    child INTEGER NOT NULL,
    ordinal INTEGER NOT NULL,
    result_payload BLOB NOT NULL CHECK(length(result_payload) BETWEEN 1 AND 65536),
    completion_sequence INTEGER NOT NULL UNIQUE CHECK(completion_sequence>0),
    success_digest BLOB NOT NULL CHECK(length(success_digest)=32),
    PRIMARY KEY(call_id,child,ordinal),
    FOREIGN KEY(call_id,child,ordinal) REFERENCES web_search_attempts(call_id,child,ordinal)
) STRICT;
PRAGMA user_version=39;
COMMIT;
