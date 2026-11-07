CREATE TABLE unsubscribe_requests (
    id TEXT PRIMARY KEY,
    archive_id TEXT NOT NULL,
    message_id TEXT,
    list_key TEXT NOT NULL,
    requested_at INTEGER NOT NULL,
    requested_by TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('accepted', 'redirected', 'refused', 'http_error', 'network')),
    status INTEGER
);
CREATE INDEX unsubscribe_requests_list ON unsubscribe_requests (archive_id, list_key, requested_at);
