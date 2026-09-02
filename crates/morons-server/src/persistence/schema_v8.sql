BEGIN IMMEDIATE;

CREATE TABLE workspace_generation_layouts (
    import_request_id BLOB PRIMARY KEY NOT NULL REFERENCES repository_import_requests(request_id),
    workspace_id BLOB NOT NULL CHECK (length(workspace_id) = 16),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    generation_id BLOB NOT NULL UNIQUE CHECK (length(generation_id) = 16),
    operation_id BLOB NOT NULL UNIQUE CHECK (length(operation_id) = 16),
    state INTEGER NOT NULL CHECK (state BETWEEN 0 AND 3),
    created_sequence INTEGER NOT NULL UNIQUE CHECK (created_sequence > 0),
    updated_sequence INTEGER NOT NULL CHECK (updated_sequence >= created_sequence),
    file_count INTEGER CHECK (file_count IS NULL OR file_count BETWEEN 0 AND 50000),
    directory_count INTEGER CHECK (directory_count IS NULL OR directory_count BETWEEN 0 AND 50000),
    logical_bytes INTEGER CHECK (logical_bytes IS NULL OR logical_bytes BETWEEN 0 AND 268435456),
    manifest_digest BLOB CHECK (manifest_digest IS NULL OR length(manifest_digest) = 32),
    CHECK (
        (state = 2 AND file_count IS NOT NULL AND directory_count IS NOT NULL
         AND logical_bytes IS NOT NULL AND manifest_digest IS NOT NULL)
        OR
        (state != 2 AND file_count IS NULL AND directory_count IS NULL
         AND logical_bytes IS NULL AND manifest_digest IS NULL)
    )
) STRICT, WITHOUT ROWID;
CREATE INDEX workspace_generation_layouts_by_state
ON workspace_generation_layouts (state, created_sequence);
CREATE UNIQUE INDEX workspace_generation_layout_active
ON workspace_generation_layouts (workspace_id) WHERE state IN (0, 1, 2);

INSERT INTO workspace_generation_layouts (
    workspace_id, session_id, import_request_id, generation_id, operation_id,
    state, created_sequence, updated_sequence
)
SELECT workspace_id, session_id, request_id, randomblob(16), randomblob(16),
       CASE state WHEN 1 THEN 1 WHEN 3 THEN 3 WHEN 4 THEN 3 ELSE 0 END,
       accepted_sequence, accepted_sequence
FROM repository_import_requests;

CREATE TABLE worktree_generation_facts (
    fact_id BLOB PRIMARY KEY NOT NULL CHECK (length(fact_id) = 16),
    fact_sequence INTEGER NOT NULL UNIQUE CHECK (fact_sequence > 0),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    workspace_id BLOB NOT NULL CHECK (length(workspace_id) = 16),
    generation_id BLOB NOT NULL UNIQUE CHECK (length(generation_id) = 16),
    predecessor_generation_id BLOB CHECK (
        predecessor_generation_id IS NULL OR length(predecessor_generation_id) = 16
    ),
    publication_kind INTEGER NOT NULL CHECK (publication_kind IN (1, 2)),
    operation_id BLOB NOT NULL UNIQUE CHECK (length(operation_id) = 16),
    file_count INTEGER NOT NULL CHECK (file_count BETWEEN 0 AND 50000),
    directory_count INTEGER NOT NULL CHECK (directory_count BETWEEN 0 AND 50000),
    logical_bytes INTEGER NOT NULL CHECK (logical_bytes BETWEEN 0 AND 268435456),
    manifest_digest BLOB NOT NULL CHECK (length(manifest_digest) = 32),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;
CREATE INDEX worktree_generation_facts_by_workspace
ON worktree_generation_facts (workspace_id, fact_sequence);

CREATE TABLE active_worktree_generations (
    workspace_id BLOB PRIMARY KEY NOT NULL CHECK (length(workspace_id) = 16),
    session_id BLOB NOT NULL UNIQUE REFERENCES session_created_facts(session_id),
    generation_id BLOB NOT NULL UNIQUE REFERENCES worktree_generation_facts(generation_id),
    updated_sequence INTEGER NOT NULL CHECK (updated_sequence > 0)
) STRICT, WITHOUT ROWID;

PRAGMA user_version = 8;

COMMIT;
