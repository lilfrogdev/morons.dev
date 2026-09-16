BEGIN IMMEDIATE;
ALTER TABLE web_model_bindings ADD COLUMN admission_count INTEGER NOT NULL DEFAULT 0 CHECK (admission_count BETWEEN 0 AND 72);
UPDATE web_model_bindings SET admission_count = (SELECT COUNT(*) FROM web_search_attempts WHERE web_search_attempts.call_id = web_model_bindings.call_id);
PRAGMA user_version = 38;
COMMIT;
