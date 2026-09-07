BEGIN IMMEDIATE;
PRAGMA defer_foreign_keys = ON;

CREATE TABLE context_checkpoints_v30 (
    checkpoint_id BLOB PRIMARY KEY NOT NULL CHECK (length(checkpoint_id) = 16),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    parent_checkpoint_id BLOB REFERENCES context_checkpoints(checkpoint_id),
    source_entry_high_water INTEGER NOT NULL CHECK (source_entry_high_water > 0),
    source_digest BLOB NOT NULL CHECK (length(source_digest) = 32),
    context_policy_version INTEGER NOT NULL CHECK (context_policy_version = 4),
    open_code_service INTEGER NOT NULL CHECK (open_code_service IN (1, 2, 3)),
    model_id TEXT NOT NULL CHECK (length(CAST(model_id AS BLOB)) BETWEEN 1 AND 128),
    summary TEXT NOT NULL CHECK (length(CAST(summary AS BLOB)) BETWEEN 1 AND 131072),
    estimated_summary_tokens INTEGER NOT NULL CHECK (estimated_summary_tokens BETWEEN 1 AND 96000),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    UNIQUE (session_id, source_entry_high_water)
) STRICT, WITHOUT ROWID;

INSERT INTO context_checkpoints_v30 SELECT * FROM context_checkpoints;
DROP TABLE context_checkpoints;
ALTER TABLE context_checkpoints_v30 RENAME TO context_checkpoints;

CREATE INDEX context_checkpoints_by_session
ON context_checkpoints (session_id, source_entry_high_water);

CREATE TABLE default_model_selections_v30 (
    request_id BLOB PRIMARY KEY NOT NULL REFERENCES mutation_requests(request_id),
    operation_fingerprint BLOB NOT NULL CHECK (length(operation_fingerprint) = 32),
    open_code_service INTEGER NOT NULL CHECK (open_code_service IN (1, 2, 3)),
    model_id TEXT NOT NULL CHECK (length(CAST(model_id AS BLOB)) BETWEEN 1 AND 128),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;

INSERT INTO default_model_selections_v30 SELECT * FROM default_model_selections;
DROP TABLE default_model_selections;
ALTER TABLE default_model_selections_v30 RENAME TO default_model_selections;

CREATE INDEX default_model_selections_by_sequence
ON default_model_selections (accepted_sequence);

CREATE TABLE provider_operation_facts_v30 (
    fact_id BLOB PRIMARY KEY NOT NULL CHECK (length(fact_id) = 16),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    operation_id BLOB NOT NULL CHECK (length(operation_id) = 16),
    run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id),
    fact_kind INTEGER NOT NULL CHECK (fact_kind IN (1, 2, 3, 4, 5, 6)),
    open_code_service INTEGER CHECK (open_code_service IS NULL OR open_code_service IN (1, 2, 3)),
    model_id TEXT CHECK (
        model_id IS NULL OR length(CAST(model_id AS BLOB)) BETWEEN 1 AND 128
    ),
    protocol_revision INTEGER CHECK (
        protocol_revision IS NULL OR protocol_revision BETWEEN 1 AND 65535
    ),
    credential_generation INTEGER CHECK (
        credential_generation IS NULL OR credential_generation > 0
    ),
    context_policy_version INTEGER CHECK (
        context_policy_version IS NULL OR context_policy_version BETWEEN 1 AND 65535
    ),
    source_entry_high_water INTEGER CHECK (
        source_entry_high_water IS NULL OR source_entry_high_water > 0
    ),
    provider_response_id TEXT CHECK (
        provider_response_id IS NULL OR length(CAST(provider_response_id AS BLOB)) BETWEEN 1 AND 128
    ),
    failure_kind INTEGER CHECK (failure_kind IS NULL OR failure_kind BETWEEN 1 AND 10 OR failure_kind IN (12, 13)),
    input_tokens INTEGER CHECK (input_tokens IS NULL OR input_tokens >= 0),
    cached_input_tokens INTEGER CHECK (cached_input_tokens IS NULL OR cached_input_tokens >= 0),
    cache_write_input_tokens INTEGER CHECK (
        cache_write_input_tokens IS NULL OR cache_write_input_tokens >= 0
    ),
    output_tokens INTEGER CHECK (output_tokens IS NULL OR output_tokens >= 0),
    reasoning_output_tokens INTEGER CHECK (
        reasoning_output_tokens IS NULL OR reasoning_output_tokens >= 0
    ),
    total_tokens INTEGER CHECK (total_tokens IS NULL OR total_tokens >= 0),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0), turn_index INTEGER
CHECK (turn_index IS NULL OR turn_index BETWEEN 1 AND 65535), tool_catalog_version INTEGER
CHECK (tool_catalog_version IS NULL OR tool_catalog_version BETWEEN 0 AND 65535), tool_limits_version INTEGER
CHECK (tool_limits_version IS NULL OR tool_limits_version BETWEEN 0 AND 65535), estimated_input_tokens INTEGER
CHECK (estimated_input_tokens IS NULL OR estimated_input_tokens BETWEEN 1 AND 96000),
    UNIQUE (operation_id, fact_kind),
    CHECK (
        (fact_kind = 1 AND open_code_service IS NOT NULL AND model_id IS NOT NULL
         AND protocol_revision IS NOT NULL AND credential_generation IS NOT NULL
         AND context_policy_version IS NOT NULL AND source_entry_high_water IS NOT NULL
         AND provider_response_id IS NULL AND failure_kind IS NULL
         AND input_tokens IS NULL AND cached_input_tokens IS NULL
         AND cache_write_input_tokens IS NULL AND output_tokens IS NULL
         AND reasoning_output_tokens IS NULL AND total_tokens IS NULL)
        OR
        (fact_kind = 3 AND open_code_service IS NULL AND model_id IS NULL
         AND protocol_revision IS NULL AND credential_generation IS NULL
         AND context_policy_version IS NULL AND source_entry_high_water IS NULL
         AND provider_response_id IS NOT NULL AND failure_kind IS NULL
         AND input_tokens IS NOT NULL AND cached_input_tokens IS NOT NULL
         AND cache_write_input_tokens IS NOT NULL AND output_tokens IS NOT NULL
         AND reasoning_output_tokens IS NOT NULL AND total_tokens IS NOT NULL)
        OR
        (fact_kind = 4 AND open_code_service IS NULL AND model_id IS NULL
         AND protocol_revision IS NULL AND credential_generation IS NULL
         AND context_policy_version IS NULL AND source_entry_high_water IS NULL
         AND provider_response_id IS NULL AND failure_kind IS NOT NULL
         AND input_tokens IS NULL AND cached_input_tokens IS NULL
         AND cache_write_input_tokens IS NULL AND output_tokens IS NULL
         AND reasoning_output_tokens IS NULL AND total_tokens IS NULL)
        OR
        (fact_kind = 5 AND open_code_service IS NULL AND model_id IS NULL
         AND protocol_revision IS NULL AND credential_generation IS NULL
         AND context_policy_version IS NULL AND source_entry_high_water IS NULL
         AND provider_response_id IS NULL
         AND input_tokens IS NULL AND cached_input_tokens IS NULL
         AND cache_write_input_tokens IS NULL AND output_tokens IS NULL
         AND reasoning_output_tokens IS NULL AND total_tokens IS NULL)
        OR
        (fact_kind IN (2, 6) AND open_code_service IS NULL AND model_id IS NULL
         AND protocol_revision IS NULL AND credential_generation IS NULL
         AND context_policy_version IS NULL AND source_entry_high_water IS NULL
         AND provider_response_id IS NULL AND failure_kind IS NULL
         AND input_tokens IS NULL AND cached_input_tokens IS NULL
         AND cache_write_input_tokens IS NULL AND output_tokens IS NULL
         AND reasoning_output_tokens IS NULL AND total_tokens IS NULL)
    )
) STRICT, WITHOUT ROWID;

INSERT INTO provider_operation_facts_v30 SELECT * FROM provider_operation_facts;
DROP TABLE provider_operation_facts;
ALTER TABLE provider_operation_facts_v30 RENAME TO provider_operation_facts;

CREATE INDEX provider_operation_facts_by_run
ON provider_operation_facts (run_id, fact_sequence);

CREATE TABLE run_accepted_facts_v30 (
    fact_id BLOB PRIMARY KEY NOT NULL CHECK (length(fact_id) = 16),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    request_id BLOB NOT NULL UNIQUE REFERENCES run_input_requests(request_id),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    run_id BLOB NOT NULL UNIQUE CHECK (length(run_id) = 16),
    user_message_id BLOB NOT NULL UNIQUE CHECK (length(user_message_id) = 16),
    open_code_service INTEGER NOT NULL CHECK (open_code_service IN (1, 2, 3)),
    model_id TEXT NOT NULL CHECK (length(CAST(model_id AS BLOB)) BETWEEN 1 AND 128),
    protocol_revision INTEGER NOT NULL CHECK (protocol_revision BETWEEN 1 AND 65535),
    credential_generation INTEGER NOT NULL CHECK (credential_generation > 0),
    context_policy_version INTEGER NOT NULL CHECK (context_policy_version BETWEEN 1 AND 65535),
    source_entry_high_water INTEGER NOT NULL CHECK (source_entry_high_water > 0),
    estimated_input_tokens INTEGER NOT NULL CHECK (estimated_input_tokens BETWEEN 1 AND 96000),
    maximum_input_tokens INTEGER NOT NULL CHECK (maximum_input_tokens BETWEEN 1 AND 96000),
    maximum_output_tokens INTEGER NOT NULL CHECK (maximum_output_tokens BETWEEN 1 AND 32000),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0),
    delivery_event_id BLOB NOT NULL UNIQUE CHECK (length(delivery_event_id) = 16)
, tool_catalog_version INTEGER NOT NULL DEFAULT 0
CHECK (tool_catalog_version BETWEEN 0 AND 65535), tool_limits_version INTEGER NOT NULL DEFAULT 0
CHECK (tool_limits_version BETWEEN 0 AND 65535), execution_image_generation BLOB
CHECK (execution_image_generation IS NULL OR length(execution_image_generation) = 16)) STRICT, WITHOUT ROWID;

INSERT INTO run_accepted_facts_v30 SELECT * FROM run_accepted_facts;
DROP TABLE run_accepted_facts;
ALTER TABLE run_accepted_facts_v30 RENAME TO run_accepted_facts;

CREATE INDEX run_accepted_facts_by_session
ON run_accepted_facts (session_id, fact_sequence);

CREATE TABLE run_state_facts_v30 (
    fact_id BLOB PRIMARY KEY NOT NULL CHECK (length(fact_id) = 16),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id),
    state INTEGER NOT NULL CHECK (state BETWEEN 2 AND 7),
    failure_kind INTEGER CHECK (failure_kind IS NULL OR failure_kind BETWEEN 1 AND 13),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    delivery_event_id BLOB NOT NULL UNIQUE CHECK (length(delivery_event_id) = 16),
    UNIQUE (run_id, state),
    CHECK ((state = 4 AND failure_kind IS NOT NULL) OR (state != 4 AND failure_kind IS NULL))
) STRICT, WITHOUT ROWID;

INSERT INTO run_state_facts_v30 SELECT * FROM run_state_facts;
DROP TABLE run_state_facts;
ALTER TABLE run_state_facts_v30 RENAME TO run_state_facts;

CREATE INDEX run_state_facts_by_run ON run_state_facts (run_id, fact_sequence);

CREATE TABLE runs_v30 (
    run_id BLOB PRIMARY KEY NOT NULL REFERENCES run_accepted_facts(run_id),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    user_message_id BLOB NOT NULL UNIQUE CHECK (length(user_message_id) = 16),
    open_code_service INTEGER NOT NULL CHECK (open_code_service IN (1, 2, 3)),
    model_id TEXT NOT NULL CHECK (length(CAST(model_id AS BLOB)) BETWEEN 1 AND 128),
    protocol_revision INTEGER NOT NULL CHECK (protocol_revision BETWEEN 1 AND 65535),
    credential_generation INTEGER NOT NULL CHECK (credential_generation > 0),
    context_policy_version INTEGER NOT NULL CHECK (context_policy_version BETWEEN 1 AND 65535),
    tool_catalog_version INTEGER NOT NULL CHECK (tool_catalog_version BETWEEN 0 AND 65535),
    tool_limits_version INTEGER NOT NULL CHECK (tool_limits_version BETWEEN 0 AND 65535),
    source_entry_high_water INTEGER NOT NULL CHECK (source_entry_high_water > 0),
    estimated_input_tokens INTEGER NOT NULL CHECK (estimated_input_tokens BETWEEN 1 AND 96000),
    maximum_input_tokens INTEGER NOT NULL CHECK (maximum_input_tokens BETWEEN 1 AND 96000),
    maximum_output_tokens INTEGER NOT NULL CHECK (maximum_output_tokens BETWEEN 1 AND 32000),
    provider_turns INTEGER NOT NULL CHECK (provider_turns BETWEEN 0 AND 65535),
    tool_calls INTEGER NOT NULL CHECK (tool_calls BETWEEN 0 AND 64),
    tool_mutations INTEGER NOT NULL CHECK (tool_mutations BETWEEN 0 AND 16),
    tool_result_bytes INTEGER NOT NULL CHECK (tool_result_bytes BETWEEN 0 AND 2097152),
    state INTEGER NOT NULL CHECK (state BETWEEN 1 AND 7),
    cancellation_requested INTEGER NOT NULL CHECK (cancellation_requested IN (0, 1)),
    failure_kind INTEGER CHECK (failure_kind IS NULL OR failure_kind BETWEEN 1 AND 13),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    updated_sequence INTEGER NOT NULL CHECK (updated_sequence >= accepted_sequence),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0),
    updated_at_milliseconds INTEGER NOT NULL CHECK (updated_at_milliseconds >= 0), execution_image_generation BLOB
CHECK (execution_image_generation IS NULL OR length(execution_image_generation) = 16),
    CHECK ((state = 4 AND failure_kind IS NOT NULL) OR (state != 4 AND failure_kind IS NULL))
) STRICT, WITHOUT ROWID;

INSERT INTO runs_v30 SELECT * FROM runs;
DROP TABLE runs;
ALTER TABLE runs_v30 RENAME TO runs;

CREATE INDEX runs_by_session ON runs (session_id, accepted_sequence);

CREATE TABLE session_entries_v30 (
    fact_id BLOB PRIMARY KEY NOT NULL CHECK (length(fact_id) = 16),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    entry_sequence INTEGER NOT NULL CHECK (entry_sequence > 0),
    message_id BLOB NOT NULL UNIQUE CHECK (length(message_id) = 16),
    run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id),
    entry_kind INTEGER NOT NULL CHECK (entry_kind BETWEEN 1 AND 4),
    actor_kind INTEGER NOT NULL CHECK (actor_kind BETWEEN 1 AND 3),
    open_code_service INTEGER CHECK (open_code_service IS NULL OR open_code_service IN (1, 2, 3)),
    model_id TEXT CHECK (
        model_id IS NULL OR length(CAST(model_id AS BLOB)) BETWEEN 1 AND 128
    ),
    text TEXT CHECK (
        text IS NULL OR length(CAST(text AS BLOB)) BETWEEN 1 AND 131072
    ),
    refusal INTEGER NOT NULL CHECK (refusal IN (0, 1)),
    assistant_phase INTEGER CHECK (assistant_phase IS NULL OR assistant_phase IN (1, 2)),
    tool_call_id BLOB CHECK (tool_call_id IS NULL OR length(tool_call_id) = 16),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    delivery_event_id BLOB NOT NULL UNIQUE CHECK (length(delivery_event_id) = 16),
    UNIQUE (session_id, entry_sequence),
    CHECK (
        (entry_kind = 1 AND actor_kind = 1 AND open_code_service IS NULL
         AND model_id IS NULL AND text IS NOT NULL AND refusal = 0
         AND assistant_phase IS NULL AND tool_call_id IS NULL)
        OR
        (entry_kind = 2 AND actor_kind = 2 AND open_code_service IS NOT NULL
         AND model_id IS NOT NULL AND text IS NOT NULL
         AND assistant_phase IS NOT NULL AND tool_call_id IS NULL)
        OR
        (entry_kind = 3 AND actor_kind = 2 AND open_code_service IS NOT NULL
         AND model_id IS NOT NULL AND text IS NULL AND refusal = 0
         AND assistant_phase IS NULL AND tool_call_id IS NOT NULL)
        OR
        (entry_kind = 4 AND actor_kind = 3 AND open_code_service IS NULL
         AND model_id IS NULL AND text IS NULL AND refusal = 0
         AND assistant_phase IS NULL AND tool_call_id IS NOT NULL)
    )
) STRICT, WITHOUT ROWID;

INSERT INTO session_entries_v30 SELECT * FROM session_entries;
DROP TABLE session_entries;
ALTER TABLE session_entries_v30 RENAME TO session_entries;

CREATE INDEX session_entries_by_session
ON session_entries (session_id, entry_sequence);

CREATE UNIQUE INDEX session_entries_user_by_run
ON session_entries (run_id) WHERE entry_kind = 1;

CREATE UNIQUE INDEX session_entries_final_assistant_by_run
ON session_entries (run_id) WHERE entry_kind = 2 AND assistant_phase = 2;

CREATE UNIQUE INDEX session_entries_tool_call_kind
ON session_entries (tool_call_id, entry_kind) WHERE tool_call_id IS NOT NULL;

CREATE TABLE subagent_model_selections_v30 (
    request_id BLOB PRIMARY KEY NOT NULL REFERENCES mutation_requests(request_id),
    operation_fingerprint BLOB NOT NULL CHECK (length(operation_fingerprint) = 32),
    selection_kind INTEGER NOT NULL CHECK (selection_kind IN (1, 2)),
    open_code_service INTEGER CHECK (open_code_service IN (1, 2, 3)),
    model_id TEXT CHECK (
        model_id IS NULL
        OR length(CAST(model_id AS BLOB)) BETWEEN 1 AND 128
    ),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0),
    CHECK (
        (selection_kind = 1 AND open_code_service IS NULL AND model_id IS NULL)
        OR
        (selection_kind = 2 AND open_code_service IS NOT NULL AND model_id IS NOT NULL)
    )
) STRICT, WITHOUT ROWID;

INSERT INTO subagent_model_selections_v30 SELECT * FROM subagent_model_selections;
DROP TABLE subagent_model_selections;
ALTER TABLE subagent_model_selections_v30 RENAME TO subagent_model_selections;

CREATE INDEX subagent_model_selections_by_sequence
ON subagent_model_selections (accepted_sequence);

CREATE TABLE provider_binding_epoch (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    first_sequence INTEGER NOT NULL CHECK (first_sequence > 0)
) STRICT;
INSERT INTO provider_binding_epoch SELECT singleton, next_value FROM logical_sequences;

CREATE TABLE task_model_bindings (
    call_id BLOB PRIMARY KEY NOT NULL REFERENCES tool_calls(call_id) ON DELETE CASCADE,
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
CREATE INDEX task_model_bindings_by_run ON task_model_bindings (run_id, dispatch_sequence);
PRAGMA user_version = 30;
COMMIT;
