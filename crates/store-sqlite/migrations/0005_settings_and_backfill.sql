CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

ALTER TABLE archives ADD COLUMN save_remote_images_since INTEGER;
UPDATE archives SET save_remote_images_since = created_at WHERE save_remote_images = 1;
CREATE INDEX messages_archived ON messages (archive_id, archived_at);
