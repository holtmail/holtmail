CREATE TABLE item_change_messages (
    outbox_id TEXT PRIMARY KEY,
    change_id TEXT NOT NULL
);
CREATE INDEX item_change_messages_change ON item_change_messages (change_id);

CREATE TABLE deferred_replies (
    message_id TEXT PRIMARY KEY,
    archive_id TEXT NOT NULL,
    since INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 1,
    next_attempt_at INTEGER NOT NULL
);
CREATE INDEX deferred_replies_due ON deferred_replies (next_attempt_at);
