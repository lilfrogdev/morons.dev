BEGIN IMMEDIATE;

ALTER TABLE steering_mutation_requests ADD COLUMN skill_context TEXT
    CHECK (skill_context IS NULL OR (change_kind IN (1, 2)
        AND length(CAST(skill_context AS BLOB)) BETWEEN 1 AND 2097152));
ALTER TABLE steering_mutation_requests ADD COLUMN skill_context_digest BLOB
    CHECK ((skill_context IS NULL AND skill_context_digest IS NULL)
        OR (skill_context IS NOT NULL AND skill_context_digest IS NOT NULL
            AND length(skill_context_digest) = 32));

PRAGMA user_version = 46;
COMMIT;
