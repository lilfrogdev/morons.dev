BEGIN IMMEDIATE;
PRAGMA defer_foreign_keys = ON;

CREATE TABLE provider_operation_facts_v34 (
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
CHECK (turn_index IS NULL OR turn_index > 0), tool_catalog_version INTEGER
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

INSERT INTO provider_operation_facts_v34 SELECT * FROM provider_operation_facts;
DROP TABLE provider_operation_facts;
ALTER TABLE provider_operation_facts_v34 RENAME TO provider_operation_facts;

CREATE INDEX provider_operation_facts_by_run
ON provider_operation_facts (run_id, fact_sequence);

CREATE TABLE runs_v34 (
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
    provider_turns INTEGER NOT NULL CHECK (provider_turns >= 0),
    tool_calls INTEGER NOT NULL CHECK (tool_calls >= 0),
    tool_mutations INTEGER NOT NULL CHECK (tool_mutations >= 0),
    tool_result_bytes INTEGER NOT NULL CHECK (tool_result_bytes >= 0),
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

INSERT INTO runs_v34 SELECT * FROM runs;
DROP TABLE runs;
ALTER TABLE runs_v34 RENAME TO runs;

CREATE INDEX runs_by_session ON runs (session_id, accepted_sequence);

PRAGMA user_version = 34;
COMMIT;
