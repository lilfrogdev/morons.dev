BEGIN IMMEDIATE;

PRAGMA defer_foreign_keys = ON;

CREATE TABLE mutation_requests_v3 (
    request_id BLOB PRIMARY KEY NOT NULL CHECK (length(request_id) = 16),
    operation_kind INTEGER NOT NULL CHECK (operation_kind IN (1, 2, 3, 4, 5)),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;

INSERT INTO mutation_requests_v3 (
    request_id,
    operation_kind,
    accepted_sequence,
    accepted_at_milliseconds
)
SELECT request_id, operation_kind, accepted_sequence, accepted_at_milliseconds
FROM mutation_requests;

DROP TABLE mutation_requests;
ALTER TABLE mutation_requests_v3 RENAME TO mutation_requests;

CREATE TABLE run_input_requests (
    request_id BLOB PRIMARY KEY NOT NULL REFERENCES mutation_requests(request_id),
    operation_fingerprint BLOB NOT NULL CHECK (length(operation_fingerprint) = 32),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    run_id BLOB NOT NULL UNIQUE CHECK (length(run_id) = 16),
    user_message_id BLOB NOT NULL UNIQUE CHECK (length(user_message_id) = 16),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;

CREATE TABLE run_accepted_facts (
    fact_id BLOB PRIMARY KEY NOT NULL CHECK (length(fact_id) = 16),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    request_id BLOB NOT NULL UNIQUE REFERENCES run_input_requests(request_id),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    run_id BLOB NOT NULL UNIQUE CHECK (length(run_id) = 16),
    user_message_id BLOB NOT NULL UNIQUE CHECK (length(user_message_id) = 16),
    open_code_service INTEGER NOT NULL CHECK (open_code_service IN (1, 2)),
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
) STRICT, WITHOUT ROWID;

CREATE INDEX run_accepted_facts_by_session
ON run_accepted_facts (session_id, fact_sequence);

CREATE TABLE session_entries (
    fact_id BLOB PRIMARY KEY NOT NULL CHECK (length(fact_id) = 16),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    entry_sequence INTEGER NOT NULL CHECK (entry_sequence > 0),
    message_id BLOB NOT NULL UNIQUE CHECK (length(message_id) = 16),
    run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id),
    entry_kind INTEGER NOT NULL CHECK (entry_kind IN (1, 2)),
    actor_kind INTEGER NOT NULL CHECK (actor_kind IN (1, 2)),
    open_code_service INTEGER CHECK (open_code_service IS NULL OR open_code_service IN (1, 2)),
    model_id TEXT CHECK (
        model_id IS NULL OR length(CAST(model_id AS BLOB)) BETWEEN 1 AND 128
    ),
    text TEXT NOT NULL CHECK (length(CAST(text AS BLOB)) BETWEEN 1 AND 131072),
    refusal INTEGER NOT NULL CHECK (refusal IN (0, 1)),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    delivery_event_id BLOB NOT NULL UNIQUE CHECK (length(delivery_event_id) = 16),
    UNIQUE (session_id, entry_sequence),
    UNIQUE (run_id, entry_kind),
    CHECK (
        (entry_kind = 1 AND actor_kind = 1 AND open_code_service IS NULL
         AND model_id IS NULL AND refusal = 0)
        OR
        (entry_kind = 2 AND actor_kind = 2 AND open_code_service IS NOT NULL
         AND model_id IS NOT NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE INDEX session_entries_by_session
ON session_entries (session_id, entry_sequence);

CREATE TABLE run_state_facts (
    fact_id BLOB PRIMARY KEY NOT NULL CHECK (length(fact_id) = 16),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id),
    state INTEGER NOT NULL CHECK (state IN (2, 3, 4, 5, 6)),
    failure_kind INTEGER CHECK (failure_kind IS NULL OR failure_kind BETWEEN 1 AND 10),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    delivery_event_id BLOB NOT NULL UNIQUE CHECK (length(delivery_event_id) = 16),
    UNIQUE (run_id, state),
    CHECK ((state = 4 AND failure_kind IS NOT NULL) OR (state != 4 AND failure_kind IS NULL))
) STRICT, WITHOUT ROWID;

CREATE INDEX run_state_facts_by_run
ON run_state_facts (run_id, fact_sequence);

CREATE TABLE run_cancellation_requests (
    request_id BLOB PRIMARY KEY NOT NULL REFERENCES mutation_requests(request_id),
    operation_fingerprint BLOB NOT NULL CHECK (length(operation_fingerprint) = 32),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0),
    result_state INTEGER NOT NULL CHECK (result_state BETWEEN 1 AND 6),
    result_cancellation_requested INTEGER NOT NULL CHECK (
        result_cancellation_requested IN (0, 1)
    ),
    intent_applied INTEGER NOT NULL CHECK (intent_applied IN (0, 1)),
    delivery_event_id BLOB UNIQUE CHECK (
        delivery_event_id IS NULL OR length(delivery_event_id) = 16
    ),
    CHECK (
        (intent_applied = 1 AND delivery_event_id IS NOT NULL)
        OR (intent_applied = 0 AND delivery_event_id IS NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX run_cancellation_intent_by_run
ON run_cancellation_requests (run_id)
WHERE intent_applied = 1;

CREATE TABLE provider_operation_facts (
    fact_id BLOB PRIMARY KEY NOT NULL CHECK (length(fact_id) = 16),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    operation_id BLOB NOT NULL CHECK (length(operation_id) = 16),
    run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id),
    fact_kind INTEGER NOT NULL CHECK (fact_kind IN (1, 2, 3, 4, 5, 6)),
    open_code_service INTEGER CHECK (open_code_service IS NULL OR open_code_service IN (1, 2)),
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
    failure_kind INTEGER CHECK (failure_kind IS NULL OR failure_kind BETWEEN 1 AND 10),
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
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
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

CREATE INDEX provider_operation_facts_by_run
ON provider_operation_facts (run_id, fact_sequence);

CREATE TABLE run_audit_facts (
    audit_id BLOB PRIMARY KEY NOT NULL CHECK (length(audit_id) = 16),
    audit_sequence INTEGER NOT NULL UNIQUE CHECK (audit_sequence > 0),
    request_id BLOB REFERENCES mutation_requests(request_id),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id),
    operation_id BLOB CHECK (operation_id IS NULL OR length(operation_id) = 16),
    actor_kind INTEGER NOT NULL CHECK (actor_kind IN (1, 2)),
    audit_kind INTEGER NOT NULL CHECK (audit_kind BETWEEN 1 AND 11),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;

CREATE INDEX run_audit_facts_by_run
ON run_audit_facts (run_id, audit_sequence);

CREATE INDEX run_audit_facts_by_operation
ON run_audit_facts (operation_id, audit_sequence)
WHERE operation_id IS NOT NULL;

CREATE TABLE runs (
    run_id BLOB PRIMARY KEY NOT NULL REFERENCES run_accepted_facts(run_id),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    user_message_id BLOB NOT NULL UNIQUE CHECK (length(user_message_id) = 16),
    open_code_service INTEGER NOT NULL CHECK (open_code_service IN (1, 2)),
    model_id TEXT NOT NULL CHECK (length(CAST(model_id AS BLOB)) BETWEEN 1 AND 128),
    protocol_revision INTEGER NOT NULL CHECK (protocol_revision BETWEEN 1 AND 65535),
    credential_generation INTEGER NOT NULL CHECK (credential_generation > 0),
    context_policy_version INTEGER NOT NULL CHECK (context_policy_version BETWEEN 1 AND 65535),
    source_entry_high_water INTEGER NOT NULL CHECK (source_entry_high_water > 0),
    estimated_input_tokens INTEGER NOT NULL CHECK (estimated_input_tokens BETWEEN 1 AND 96000),
    maximum_input_tokens INTEGER NOT NULL CHECK (maximum_input_tokens BETWEEN 1 AND 96000),
    maximum_output_tokens INTEGER NOT NULL CHECK (maximum_output_tokens BETWEEN 1 AND 32000),
    state INTEGER NOT NULL CHECK (state BETWEEN 1 AND 6),
    cancellation_requested INTEGER NOT NULL CHECK (cancellation_requested IN (0, 1)),
    failure_kind INTEGER CHECK (failure_kind IS NULL OR failure_kind BETWEEN 1 AND 10),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    updated_sequence INTEGER NOT NULL CHECK (updated_sequence >= accepted_sequence),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0),
    updated_at_milliseconds INTEGER NOT NULL CHECK (updated_at_milliseconds >= 0),
    CHECK ((state = 4 AND failure_kind IS NOT NULL) OR (state != 4 AND failure_kind IS NULL))
) STRICT, WITHOUT ROWID;

CREATE INDEX runs_by_session
ON runs (session_id, accepted_sequence);

CREATE TABLE session_run_states (
    session_id BLOB PRIMARY KEY NOT NULL REFERENCES session_created_facts(session_id),
    active_run_id BLOB UNIQUE REFERENCES run_accepted_facts(run_id),
    entry_high_water INTEGER NOT NULL CHECK (entry_high_water >= 0),
    updated_sequence INTEGER NOT NULL CHECK (updated_sequence > 0)
) STRICT, WITHOUT ROWID;

INSERT INTO session_run_states (
    session_id,
    active_run_id,
    entry_high_water,
    updated_sequence
)
SELECT session_id, NULL, 0, updated_sequence
FROM sessions;

CREATE TABLE delivery_events_v3 (
    event_id BLOB PRIMARY KEY NOT NULL CHECK (length(event_id) = 16),
    event_sequence INTEGER NOT NULL UNIQUE CHECK (event_sequence > 0),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    event_kind INTEGER NOT NULL CHECK (event_kind BETWEEN 1 AND 10),
    payload_version INTEGER NOT NULL CHECK (payload_version = 1),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;

INSERT INTO delivery_events_v3 (
    event_id,
    event_sequence,
    session_id,
    event_kind,
    payload_version,
    created_at_milliseconds
)
SELECT
    event_id,
    event_sequence,
    session_id,
    event_kind,
    payload_version,
    created_at_milliseconds
FROM delivery_events;

DROP TABLE delivery_events;
ALTER TABLE delivery_events_v3 RENAME TO delivery_events;

CREATE INDEX delivery_events_by_session
ON delivery_events (session_id, event_sequence);

PRAGMA user_version = 3;

COMMIT;
