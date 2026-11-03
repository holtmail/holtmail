ALTER TABLE archives ADD COLUMN save_remote_images INTEGER NOT NULL DEFAULT 0;

CREATE TABLE remote_images (
    key TEXT PRIMARY KEY,
    url TEXT NOT NULL,
    blob_id TEXT,
    content_type TEXT,
    size INTEGER NOT NULL DEFAULT 0,
    fetched_at INTEGER NOT NULL,
    error TEXT
);
CREATE INDEX remote_images_blob ON remote_images (blob_id);

CREATE TABLE message_remote_images (
    message_id TEXT NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
    key TEXT NOT NULL REFERENCES remote_images (key),
    PRIMARY KEY (message_id, key)
);
CREATE INDEX message_remote_images_key ON message_remote_images (key);

CREATE TABLE remote_scans (
    message_id TEXT PRIMARY KEY REFERENCES messages (id) ON DELETE CASCADE,
    scanned_at INTEGER NOT NULL
);
