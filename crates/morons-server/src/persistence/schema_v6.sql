BEGIN IMMEDIATE;

PRAGMA defer_foreign_keys = ON;

CREATE TABLE mutation_requests_v6 (
    request_id BLOB PRIMARY KEY NOT NULL CHECK (length(request_id) = 16),
    operation_kind INTEGER NOT NULL CHECK (operation_kind IN (1, 2, 3, 4, 5, 6, 7, 8)),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;

INSERT INTO mutation_requests_v6
SELECT request_id, operation_kind, accepted_sequence, accepted_at_milliseconds
FROM mutation_requests;
DROP TABLE mutation_requests;
ALTER TABLE mutation_requests_v6 RENAME TO mutation_requests;

ALTER TABLE run_accepted_facts
ADD COLUMN tool_catalog_version INTEGER NOT NULL DEFAULT 0
CHECK (tool_catalog_version BETWEEN 0 AND 65535);
ALTER TABLE run_accepted_facts
ADD COLUMN tool_limits_version INTEGER NOT NULL DEFAULT 0
CHECK (tool_limits_version BETWEEN 0 AND 65535);

ALTER TABLE provider_operation_facts
ADD COLUMN turn_index INTEGER
CHECK (turn_index IS NULL OR turn_index BETWEEN 1 AND 65535);
ALTER TABLE provider_operation_facts
ADD COLUMN tool_catalog_version INTEGER
CHECK (tool_catalog_version IS NULL OR tool_catalog_version BETWEEN 0 AND 65535);
ALTER TABLE provider_operation_facts
ADD COLUMN tool_limits_version INTEGER
CHECK (tool_limits_version IS NULL OR tool_limits_version BETWEEN 0 AND 65535);
ALTER TABLE provider_operation_facts
ADD COLUMN estimated_input_tokens INTEGER
CHECK (estimated_input_tokens IS NULL OR estimated_input_tokens BETWEEN 1 AND 96000);

UPDATE provider_operation_facts
SET turn_index = 1,
    tool_catalog_version = 0,
    tool_limits_version = 0,
    estimated_input_tokens = (
        SELECT accepted.estimated_input_tokens
        FROM run_accepted_facts AS accepted
        WHERE accepted.run_id = provider_operation_facts.run_id
    )
WHERE fact_kind = 1;

CREATE TABLE session_entries_v6 (
    fact_id BLOB PRIMARY KEY NOT NULL CHECK (length(fact_id) = 16),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    entry_sequence INTEGER NOT NULL CHECK (entry_sequence > 0),
    message_id BLOB NOT NULL UNIQUE CHECK (length(message_id) = 16),
    run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id),
    entry_kind INTEGER NOT NULL CHECK (entry_kind BETWEEN 1 AND 4),
    actor_kind INTEGER NOT NULL CHECK (actor_kind BETWEEN 1 AND 3),
    open_code_service INTEGER CHECK (open_code_service IS NULL OR open_code_service IN (1, 2)),
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

INSERT INTO session_entries_v6 (
    fact_id, fact_sequence, session_id, entry_sequence, message_id, run_id,
    entry_kind, actor_kind, open_code_service, model_id, text, refusal,
    assistant_phase, tool_call_id, created_at_milliseconds, delivery_event_id
)
SELECT
    fact_id, fact_sequence, session_id, entry_sequence, message_id, run_id,
    entry_kind, actor_kind, open_code_service, model_id, text, refusal,
    CASE entry_kind WHEN 2 THEN 2 ELSE NULL END,
    NULL, created_at_milliseconds, delivery_event_id
FROM session_entries;
DROP TABLE session_entries;
ALTER TABLE session_entries_v6 RENAME TO session_entries;
CREATE INDEX session_entries_by_session
ON session_entries (session_id, entry_sequence);
CREATE UNIQUE INDEX session_entries_user_by_run
ON session_entries (run_id) WHERE entry_kind = 1;
CREATE UNIQUE INDEX session_entries_final_assistant_by_run
ON session_entries (run_id) WHERE entry_kind = 2 AND assistant_phase = 2;
CREATE UNIQUE INDEX session_entries_tool_call_kind
ON session_entries (tool_call_id, entry_kind) WHERE tool_call_id IS NOT NULL;

CREATE TABLE run_state_facts_v6 (
    fact_id BLOB PRIMARY KEY NOT NULL CHECK (length(fact_id) = 16),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id),
    state INTEGER NOT NULL CHECK (state BETWEEN 2 AND 7),
    failure_kind INTEGER CHECK (failure_kind IS NULL OR failure_kind BETWEEN 1 AND 11),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    delivery_event_id BLOB NOT NULL UNIQUE CHECK (length(delivery_event_id) = 16),
    UNIQUE (run_id, state),
    CHECK ((state = 4 AND failure_kind IS NOT NULL) OR (state != 4 AND failure_kind IS NULL))
) STRICT, WITHOUT ROWID;
INSERT INTO run_state_facts_v6 SELECT * FROM run_state_facts;
DROP TABLE run_state_facts;
ALTER TABLE run_state_facts_v6 RENAME TO run_state_facts;
CREATE INDEX run_state_facts_by_run ON run_state_facts (run_id, fact_sequence);

CREATE TABLE runs_v6 (
    run_id BLOB PRIMARY KEY NOT NULL REFERENCES run_accepted_facts(run_id),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    user_message_id BLOB NOT NULL UNIQUE CHECK (length(user_message_id) = 16),
    open_code_service INTEGER NOT NULL CHECK (open_code_service IN (1, 2)),
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
    failure_kind INTEGER CHECK (failure_kind IS NULL OR failure_kind BETWEEN 1 AND 11),
    accepted_sequence INTEGER NOT NULL UNIQUE CHECK (accepted_sequence > 0),
    updated_sequence INTEGER NOT NULL CHECK (updated_sequence >= accepted_sequence),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0),
    updated_at_milliseconds INTEGER NOT NULL CHECK (updated_at_milliseconds >= 0),
    CHECK ((state = 4 AND failure_kind IS NOT NULL) OR (state != 4 AND failure_kind IS NULL))
) STRICT, WITHOUT ROWID;

INSERT INTO runs_v6 (
    run_id, session_id, user_message_id, open_code_service, model_id,
    protocol_revision, credential_generation, context_policy_version,
    tool_catalog_version, tool_limits_version, source_entry_high_water, estimated_input_tokens,
    maximum_input_tokens, maximum_output_tokens, provider_turns, tool_calls,
    tool_mutations, tool_result_bytes, state, cancellation_requested, failure_kind,
    accepted_sequence, updated_sequence, accepted_at_milliseconds,
    updated_at_milliseconds
)
SELECT
    run_id, session_id, user_message_id, open_code_service, model_id,
    protocol_revision, credential_generation, context_policy_version,
    0, 0, source_entry_high_water, estimated_input_tokens, maximum_input_tokens,
    maximum_output_tokens,
    CASE WHEN EXISTS (SELECT 1 FROM provider_operation_facts AS provider
                      WHERE provider.run_id = runs.run_id AND provider.fact_kind = 1)
         THEN 1 ELSE 0 END,
    0, 0, 0, state, cancellation_requested, failure_kind,
    accepted_sequence, updated_sequence, accepted_at_milliseconds,
    updated_at_milliseconds
FROM runs;
DROP TABLE runs;
ALTER TABLE runs_v6 RENAME TO runs;
CREATE INDEX runs_by_session ON runs (session_id, accepted_sequence);

CREATE TABLE tool_calls (
    call_id BLOB PRIMARY KEY NOT NULL CHECK (length(call_id) = 16),
    operation_id BLOB NOT NULL UNIQUE CHECK (length(operation_id) = 16),
    provider_operation_id BLOB NOT NULL CHECK (length(provider_operation_id) = 16),
    provider_call_id TEXT NOT NULL CHECK (
        length(CAST(provider_call_id AS BLOB)) BETWEEN 1 AND 128
    ),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id),
    call_index INTEGER NOT NULL CHECK (call_index BETWEEN 1 AND 8),
    tool_kind INTEGER NOT NULL CHECK (tool_kind BETWEEN 1 AND 6),
    input_version INTEGER NOT NULL CHECK (input_version = 1),
    input_payload BLOB NOT NULL CHECK (length(input_payload) BETWEEN 2 AND 524288),
    path_digest BLOB NOT NULL CHECK (length(path_digest) = 32),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    UNIQUE (run_id, provider_call_id),
    UNIQUE (provider_operation_id, call_index)
) STRICT, WITHOUT ROWID;
CREATE INDEX tool_calls_by_run ON tool_calls (run_id, fact_sequence);

CREATE TABLE tool_operation_facts (
    fact_id BLOB PRIMARY KEY NOT NULL CHECK (length(fact_id) = 16),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    call_id BLOB NOT NULL REFERENCES tool_calls(call_id),
    operation_id BLOB NOT NULL CHECK (length(operation_id) = 16),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id),
    fact_kind INTEGER NOT NULL CHECK (fact_kind BETWEEN 1 AND 6),
    recovery_version INTEGER CHECK (recovery_version IS NULL OR recovery_version = 1),
    recovery_payload BLOB CHECK (
        recovery_payload IS NULL OR length(recovery_payload) BETWEEN 2 AND 524288
    ),
    result_version INTEGER CHECK (result_version IS NULL OR result_version = 1),
    result_payload BLOB CHECK (
        result_payload IS NULL OR length(result_payload) BETWEEN 2 AND 524288
    ),
    result_status INTEGER CHECK (result_status IS NULL OR result_status BETWEEN 1 AND 4),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    workspace_delivery_event_id BLOB UNIQUE CHECK (
        workspace_delivery_event_id IS NULL OR length(workspace_delivery_event_id) = 16
    ),
    UNIQUE (operation_id, fact_kind),
    CHECK (
        (fact_kind = 1 AND result_version IS NULL AND result_payload IS NULL
         AND result_status IS NULL AND workspace_delivery_event_id IS NULL)
        OR
        (fact_kind = 2 AND recovery_version IS NULL AND recovery_payload IS NULL
         AND result_version IS NULL AND result_payload IS NULL
         AND result_status IS NULL AND workspace_delivery_event_id IS NULL)
        OR
        (fact_kind BETWEEN 3 AND 5 AND recovery_version IS NULL AND recovery_payload IS NULL
         AND result_version IS NOT NULL AND result_payload IS NOT NULL
         AND result_status IS NOT NULL AND workspace_delivery_event_id IS NULL)
        OR
        (fact_kind = 6 AND recovery_version IS NULL AND recovery_payload IS NULL
         AND result_version IS NOT NULL AND result_payload IS NOT NULL
         AND result_status = 4 AND workspace_delivery_event_id IS NOT NULL)
    ),
    CHECK ((recovery_version IS NULL) = (recovery_payload IS NULL))
) STRICT, WITHOUT ROWID;
CREATE INDEX tool_operation_facts_by_run
ON tool_operation_facts (run_id, fact_sequence);
CREATE UNIQUE INDEX tool_operation_terminal_by_call
ON tool_operation_facts (call_id) WHERE fact_kind BETWEEN 3 AND 6;

CREATE TABLE tool_uncertainty_acknowledgements (
    request_id BLOB PRIMARY KEY NOT NULL REFERENCES mutation_requests(request_id),
    operation_fingerprint BLOB NOT NULL CHECK (length(operation_fingerprint) = 32),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    accepted_at_milliseconds INTEGER NOT NULL CHECK (accepted_at_milliseconds >= 0),
    delivery_event_id BLOB NOT NULL UNIQUE CHECK (length(delivery_event_id) = 16),
    UNIQUE (run_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE tool_audit_facts (
    audit_id BLOB PRIMARY KEY NOT NULL CHECK (length(audit_id) = 16),
    audit_sequence INTEGER NOT NULL UNIQUE CHECK (audit_sequence > 0),
    call_id BLOB REFERENCES tool_calls(call_id),
    request_id BLOB REFERENCES tool_uncertainty_acknowledgements(request_id),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id),
    operation_id BLOB CHECK (operation_id IS NULL OR length(operation_id) = 16),
    tool_kind INTEGER CHECK (tool_kind IS NULL OR tool_kind BETWEEN 1 AND 6),
    audit_kind INTEGER NOT NULL CHECK (audit_kind BETWEEN 1 AND 6),
    path_digest BLOB CHECK (path_digest IS NULL OR length(path_digest) = 32),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    CHECK (
        (audit_kind BETWEEN 1 AND 5 AND call_id IS NOT NULL AND request_id IS NULL
         AND operation_id IS NOT NULL AND tool_kind IS NOT NULL AND path_digest IS NOT NULL)
        OR
        (audit_kind = 6 AND call_id IS NULL AND request_id IS NOT NULL
         AND operation_id IS NULL AND tool_kind IS NULL AND path_digest IS NULL)
    )
) STRICT, WITHOUT ROWID;
CREATE INDEX tool_audit_facts_by_run ON tool_audit_facts (run_id, audit_sequence);

CREATE TABLE delivery_events_v6 (
    event_id BLOB PRIMARY KEY NOT NULL CHECK (length(event_id) = 16),
    event_sequence INTEGER NOT NULL UNIQUE CHECK (event_sequence > 0),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    event_kind INTEGER NOT NULL CHECK (event_kind BETWEEN 1 AND 15),
    payload_version INTEGER NOT NULL CHECK (payload_version = 1),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;
INSERT INTO delivery_events_v6 SELECT * FROM delivery_events;
DROP TABLE delivery_events;
ALTER TABLE delivery_events_v6 RENAME TO delivery_events;
CREATE INDEX delivery_events_by_session
ON delivery_events (session_id, event_sequence);

PRAGMA user_version = 6;

COMMIT;
