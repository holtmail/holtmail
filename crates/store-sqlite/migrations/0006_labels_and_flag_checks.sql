CREATE TABLE labels (
    id INTEGER PRIMARY KEY,
    archive_id TEXT NOT NULL REFERENCES archives (id),
    name TEXT NOT NULL,
    UNIQUE (archive_id, name)
);

CREATE TABLE message_labels (
    message_id TEXT NOT NULL REFERENCES messages (id),
    label_id INTEGER NOT NULL REFERENCES labels (id),
    PRIMARY KEY (message_id, label_id)
);
CREATE INDEX message_labels_label ON message_labels (label_id, message_id);

ALTER TABLE folder_cursors ADD COLUMN flags_checked_at INTEGER;
