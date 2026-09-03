BEGIN IMMEDIATE;

CREATE TABLE image_attachments (
    attachment_id BLOB PRIMARY KEY NOT NULL CHECK (length(attachment_id) = 16),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id),
    user_message_id BLOB NOT NULL REFERENCES session_entries(message_id),
    attachment_index INTEGER NOT NULL CHECK (attachment_index BETWEEN 1 AND 4),
    display_name TEXT NOT NULL CHECK (length(CAST(display_name AS BLOB)) BETWEEN 1 AND 128),
    marker_start INTEGER NOT NULL CHECK (marker_start BETWEEN 0 AND 65535),
    media_type INTEGER NOT NULL CHECK (media_type BETWEEN 1 AND 3),
    width INTEGER NOT NULL CHECK (width BETWEEN 1 AND 2048),
    height INTEGER NOT NULL CHECK (height BETWEEN 1 AND 2048),
    byte_count INTEGER NOT NULL CHECK (byte_count BETWEEN 1 AND 2097152),
    sha256 BLOB NOT NULL CHECK (length(sha256) = 32),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0),
    UNIQUE (run_id, attachment_index),
    UNIQUE (run_id, attachment_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX image_attachments_by_message
ON image_attachments (session_id, user_message_id, attachment_index);

CREATE TABLE tool_image_attachments (
    attachment_id BLOB PRIMARY KEY NOT NULL CHECK (length(attachment_id) = 16),
    call_id BLOB NOT NULL UNIQUE REFERENCES tool_calls(call_id),
    session_id BLOB NOT NULL REFERENCES session_created_facts(session_id),
    run_id BLOB NOT NULL REFERENCES run_accepted_facts(run_id),
    display_name TEXT NOT NULL CHECK (length(CAST(display_name AS BLOB)) BETWEEN 1 AND 128),
    media_type INTEGER NOT NULL CHECK (media_type BETWEEN 1 AND 3),
    width INTEGER NOT NULL CHECK (width BETWEEN 1 AND 2048),
    height INTEGER NOT NULL CHECK (height BETWEEN 1 AND 2048),
    byte_count INTEGER NOT NULL CHECK (byte_count BETWEEN 1 AND 2097152),
    sha256 BLOB NOT NULL CHECK (length(sha256) = 32),
    created_at_milliseconds INTEGER NOT NULL CHECK (created_at_milliseconds >= 0)
) STRICT, WITHOUT ROWID;
CREATE INDEX tool_image_attachments_by_run
ON tool_image_attachments (run_id, call_id);

PRAGMA user_version = 18;

COMMIT;
