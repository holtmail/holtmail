ALTER TABLE items ADD COLUMN schedule_tag TEXT;

CREATE TABLE schedule_inbox (
    message_id TEXT NOT NULL,
    part INTEGER NOT NULL,
    archive_id TEXT NOT NULL,
    method TEXT NOT NULL,
    uid TEXT NOT NULL,
    note TEXT NOT NULL,
    received_at INTEGER NOT NULL,
    PRIMARY KEY (message_id, part)
);
CREATE INDEX schedule_inbox_archive ON schedule_inbox (archive_id, received_at);

CREATE TABLE schedule_inbox_hidden (
    message_id TEXT NOT NULL,
    part INTEGER NOT NULL,
    user_id TEXT NOT NULL,
    PRIMARY KEY (message_id, part, user_id)
);

CREATE TABLE item_changes_next (
    id TEXT PRIMARY KEY,
    collection_id TEXT NOT NULL REFERENCES collections (id),
    archive_id TEXT NOT NULL REFERENCES archives (id),
    item_id TEXT,
    href TEXT,
    op TEXT NOT NULL CHECK (op IN ('create', 'update', 'delete')),
    class TEXT NOT NULL CHECK (class IN ('noop', 'personal', 'reply', 'edit', 'create', 'delete')),
    blob_id TEXT,
    content_type TEXT,
    patch TEXT,
    base_version INTEGER,
    base_etag TEXT,
    supersedes TEXT,
    origin TEXT NOT NULL CHECK (origin IN ('dav', 'api', 'itip')),
    user_id TEXT,
    device TEXT,
    created_at INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('held', 'sending', 'applied', 'superseded', 'conflict',
                                         'rejected', 'discarded')),
    reason TEXT,
    upstream_changed INTEGER NOT NULL DEFAULT 0,
    upstream_deleted INTEGER NOT NULL DEFAULT 0,
    rebased_onto INTEGER,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER,
    last_error TEXT,
    updated_at INTEGER NOT NULL,
    CHECK (item_id IS NOT NULL OR href IS NOT NULL)
);
INSERT INTO item_changes_next (id, collection_id, archive_id, item_id, href, op, class, blob_id,
    content_type, patch, base_version, base_etag, supersedes, origin, user_id, device, created_at,
    state, reason, upstream_changed, upstream_deleted, rebased_onto, attempts, next_attempt_at,
    last_error, updated_at)
SELECT id, collection_id, archive_id, item_id, href, op, class, blob_id,
    content_type, patch, base_version, base_etag, supersedes, origin, user_id, device, created_at,
    state, reason, upstream_changed, upstream_deleted, rebased_onto, attempts, next_attempt_at,
    last_error, updated_at
FROM item_changes;
DROP TABLE item_changes;
ALTER TABLE item_changes_next RENAME TO item_changes;

CREATE INDEX item_changes_item ON item_changes (item_id, id) WHERE item_id IS NOT NULL;
CREATE INDEX item_changes_href ON item_changes (collection_id, href, id) WHERE href IS NOT NULL;
CREATE INDEX item_changes_open ON item_changes (collection_id, state, id)
    WHERE state IN ('held', 'sending', 'conflict', 'rejected');
CREATE INDEX item_changes_archive_open ON item_changes (archive_id, id)
    WHERE state IN ('held', 'sending', 'conflict', 'rejected');
CREATE INDEX item_changes_supersedes ON item_changes (supersedes) WHERE supersedes IS NOT NULL;
CREATE INDEX item_changes_blob ON item_changes (blob_id) WHERE blob_id IS NOT NULL;
