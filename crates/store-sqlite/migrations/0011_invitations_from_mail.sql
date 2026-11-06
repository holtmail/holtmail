ALTER TABLE item_versions ADD COLUMN from_mail INTEGER NOT NULL DEFAULT 0;
ALTER TABLE item_versions ADD COLUMN method TEXT;
ALTER TABLE item_versions ADD COLUMN sequence INTEGER;
ALTER TABLE item_versions ADD COLUMN dtstamp INTEGER;
ALTER TABLE item_versions ADD COLUMN sent_at INTEGER;

CREATE TABLE item_version_messages (
    item_id TEXT NOT NULL REFERENCES items (id),
    blob_id TEXT NOT NULL,
    message_id TEXT NOT NULL REFERENCES messages (id),
    PRIMARY KEY (item_id, blob_id, message_id)
) WITHOUT ROWID;
CREATE INDEX item_version_messages_message ON item_version_messages (message_id);
