ALTER TABLE changes ADD COLUMN kind TEXT NOT NULL DEFAULT 'item'
    CHECK (kind IN ('item', 'message', 'place', 'flags', 'thread', 'mailbox', 'submission',
                    'draft'));
ALTER TABLE changes ADD COLUMN message_id TEXT;
ALTER TABLE changes ADD COLUMN folder_id TEXT;
ALTER TABLE changes ADD COLUMN user_id TEXT;
ALTER TABLE changes ADD COLUMN thread_id TEXT;
ALTER TABLE changes ADD COLUMN outbox_id TEXT;
ALTER TABLE changes ADD COLUMN draft_id TEXT;

CREATE INDEX locations_folder_removed ON locations (folder_id, removed_at);

CREATE TABLE local_flags (
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    message_id TEXT NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
    archive_id TEXT NOT NULL,
    flag_key TEXT NOT NULL,
    flag TEXT NOT NULL,
    present INTEGER NOT NULL CHECK (present IN (0, 1)),
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, message_id, flag_key)
) WITHOUT ROWID;
CREATE INDEX local_flags_archive ON local_flags (user_id, archive_id, message_id);
CREATE INDEX local_flags_message ON local_flags (message_id);

CREATE TABLE local_places (
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    archive_id TEXT NOT NULL,
    role TEXT NOT NULL,
    message_id TEXT NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, role, message_id)
) WITHOUT ROWID;
CREATE INDEX local_places_archive ON local_places (user_id, archive_id, role, created_at);
CREATE INDEX local_places_message ON local_places (message_id);

CREATE TABLE served_subscriptions (
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    archive_id TEXT NOT NULL,
    mailbox TEXT NOT NULL,
    subscribed INTEGER NOT NULL CHECK (subscribed IN (0, 1)),
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, archive_id, mailbox)
) WITHOUT ROWID;

ALTER TABLE draft_versions ADD COLUMN raw_blob_id TEXT;
