BEGIN IMMEDIATE;
ALTER TABLE web_model_bindings ADD COLUMN exa_contract_revision INTEGER NOT NULL DEFAULT 0 CHECK (exa_contract_revision IN (0, 1));
PRAGMA user_version = 34;
COMMIT;
