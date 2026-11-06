CREATE TABLE message_insights (
    message_id TEXT PRIMARY KEY REFERENCES messages (id),
    archive_id TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('ok', 'partial', 'unparsable', 'unreadable')),
    truncated INTEGER NOT NULL DEFAULT 0,
    from_address TEXT,
    signals INTEGER NOT NULL,
    kinds INTEGER NOT NULL,
    auth TEXT NOT NULL,
    items TEXT NOT NULL,
    computed_at INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX message_insights_fp ON message_insights (archive_id, fingerprint);
