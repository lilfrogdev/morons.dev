BEGIN IMMEDIATE;
ALTER TABLE context_accounting_epoch ADD COLUMN repeated_first_sequence INTEGER NOT NULL DEFAULT 1 CHECK (repeated_first_sequence > 0);
UPDATE context_accounting_epoch SET repeated_first_sequence = (SELECT next_value FROM logical_sequences WHERE singleton = 1);
PRAGMA user_version = 40;
COMMIT;
