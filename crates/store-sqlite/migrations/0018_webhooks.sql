CREATE TABLE webhooks (
    id TEXT PRIMARY KEY,
    url TEXT NOT NULL,
    events TEXT NOT NULL,
    sealed_secret BLOB NOT NULL,
    enabled INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE webhook_deliveries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    webhook_id TEXT NOT NULL REFERENCES webhooks (id),
    event TEXT NOT NULL,
    event_id TEXT NOT NULL,
    payload BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_at INTEGER,
    last_status INTEGER,
    last_error TEXT,
    delivered_at INTEGER
);
CREATE INDEX webhook_deliveries_due ON webhook_deliveries (next_at, id) WHERE next_at IS NOT NULL;
CREATE INDEX webhook_deliveries_hook ON webhook_deliveries (webhook_id, id);
