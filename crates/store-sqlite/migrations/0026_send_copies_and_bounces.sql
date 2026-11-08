-- no-transaction
PRAGMA foreign_keys = OFF;

BEGIN;

CREATE TABLE transports_new (
    id TEXT PRIMARY KEY,
    archive_id TEXT REFERENCES archives (id),
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('smtp', 'jmap', 'graph', 'account')),
    account_id TEXT REFERENCES accounts (id),
    settings TEXT NOT NULL,
    sealed_secret BLOB,
    allowed_from TEXT NOT NULL,
    max_size INTEGER,
    max_recipients INTEGER,
    per_hour INTEGER,
    enabled INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
INSERT INTO transports_new SELECT id, archive_id, name, kind, account_id, settings, sealed_secret,
    allowed_from, max_size, max_recipients, per_hour, enabled, created_at FROM transports;
DROP TABLE transports;
ALTER TABLE transports_new RENAME TO transports;
CREATE INDEX transports_archive ON transports (archive_id);

ALTER TABLE outbox ADD COLUMN synced_copy TEXT;
ALTER TABLE outbox ADD COLUMN note TEXT;
CREATE INDEX outbox_message ON outbox (message_id) WHERE message_id IS NOT NULL;
CREATE INDEX outbox_synced_copy ON outbox (synced_copy) WHERE synced_copy IS NOT NULL;

ALTER TABLE identities ADD COLUMN rewritten_from TEXT;
ALTER TABLE identities ADD COLUMN rewritten_at INTEGER;

COMMIT;

PRAGMA foreign_keys = ON;
