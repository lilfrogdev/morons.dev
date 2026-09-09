BEGIN IMMEDIATE;
CREATE TABLE web_binding_epoch (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    first_sequence INTEGER NOT NULL CHECK (first_sequence > 0)
) STRICT;
INSERT INTO web_binding_epoch SELECT singleton, next_value FROM logical_sequences;
CREATE TABLE web_model_bindings (
    call_id BLOB PRIMARY KEY NOT NULL REFERENCES tool_calls(call_id) CHECK (length(call_id) = 16),
    run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id) CHECK (length(run_id) = 16),
    operation_id BLOB NOT NULL CHECK (length(operation_id) = 16),
    service INTEGER NOT NULL CHECK (service = 3),
    model_id TEXT NOT NULL CHECK (model_id = 'gpt-5.5'),
    contract_revision INTEGER NOT NULL CHECK (contract_revision = 1),
    credential_kind INTEGER NOT NULL CHECK (credential_kind = 2),
    credential_generation INTEGER NOT NULL CHECK (credential_generation >= 0),
    policy_sequence INTEGER NOT NULL CHECK (policy_sequence >= 0),
    dispatch_sequence INTEGER NOT NULL UNIQUE CHECK (dispatch_sequence > 0),
    binding_digest BLOB NOT NULL CHECK (length(binding_digest) = 32)
) STRICT, WITHOUT ROWID;
CREATE INDEX web_model_bindings_by_run ON web_model_bindings(run_id, dispatch_sequence);
PRAGMA user_version = 32;
COMMIT;
