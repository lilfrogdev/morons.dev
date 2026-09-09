BEGIN IMMEDIATE;
PRAGMA defer_foreign_keys = ON;
CREATE TABLE task_model_bindings_v31 (
    call_id BLOB PRIMARY KEY NOT NULL REFERENCES tool_calls(call_id),
    run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id),
    operation_id BLOB NOT NULL CHECK (length(operation_id) = 16),
    service INTEGER NOT NULL CHECK (service IN (1, 2, 3)),
    model_id TEXT NOT NULL CHECK (length(CAST(model_id AS BLOB)) BETWEEN 1 AND 128),
    credential_kind INTEGER NOT NULL CHECK ((service IN (1, 2) AND credential_kind = 1) OR (service = 3 AND credential_kind = 2)),
    credential_generation INTEGER NOT NULL CHECK (credential_generation > 0),
    protocol_revision INTEGER NOT NULL CHECK (protocol_revision BETWEEN 1 AND 65535),
    maximum_input_tokens INTEGER NOT NULL CHECK (maximum_input_tokens BETWEEN 1 AND 96000),
    maximum_output_tokens INTEGER NOT NULL CHECK (maximum_output_tokens BETWEEN 1 AND 32000),
    policy_sequence INTEGER NOT NULL CHECK (policy_sequence >= 0),
    dispatch_sequence INTEGER NOT NULL UNIQUE CHECK (dispatch_sequence > policy_sequence),
    binding_digest BLOB NOT NULL CHECK (length(binding_digest) = 32)
) STRICT, WITHOUT ROWID;
INSERT INTO task_model_bindings_v31 SELECT * FROM task_model_bindings;
DROP TABLE task_model_bindings;
ALTER TABLE task_model_bindings_v31 RENAME TO task_model_bindings;
CREATE INDEX task_model_bindings_by_run ON task_model_bindings(run_id, dispatch_sequence);
PRAGMA user_version = 31;
COMMIT;
