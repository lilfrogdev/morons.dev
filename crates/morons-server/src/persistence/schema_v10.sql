BEGIN IMMEDIATE;

ALTER TABLE repository_import_requests
ADD COLUMN review_baseline_version INTEGER NOT NULL DEFAULT 0
CHECK (review_baseline_version IN (0, 1));

PRAGMA user_version = 10;

COMMIT;
