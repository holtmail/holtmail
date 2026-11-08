ALTER TABLE archive_grants ADD COLUMN role TEXT NOT NULL DEFAULT 'reader'
    CHECK (role IN ('reader', 'editor', 'sender'));

CREATE TABLE account_capabilities (
    account_id TEXT NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    capability TEXT NOT NULL,
    PRIMARY KEY (account_id, capability)
) WITHOUT ROWID;

CREATE TABLE audit_log (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    at INTEGER NOT NULL,
    user_id TEXT,
    kind TEXT NOT NULL,
    archive_id TEXT,
    account_id TEXT,
    targets TEXT NOT NULL,
    count INTEGER NOT NULL
);
CREATE INDEX audit_log_archive ON audit_log (archive_id, seq);

CREATE TABLE idempotency_keys (
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    route TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    status INTEGER,
    content_type TEXT,
    body BLOB,
    PRIMARY KEY (user_id, key, route)
) WITHOUT ROWID;
CREATE INDEX idempotency_keys_created ON idempotency_keys (created_at);
