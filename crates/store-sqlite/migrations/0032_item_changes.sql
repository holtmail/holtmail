ALTER TABLE collections ADD COLUMN write_mode TEXT NOT NULL DEFAULT 'read_only'
    CHECK (write_mode IN ('read_only', 'held', 'write_through'));
UPDATE collections SET write_mode = 'held' WHERE account_id IS NOT NULL;

ALTER TABLE items ADD COLUMN dav_name TEXT;
CREATE UNIQUE INDEX items_dav_name ON items (collection_id, dav_name) WHERE dav_name IS NOT NULL;

ALTER TABLE item_versions ADD COLUMN origin TEXT NOT NULL DEFAULT 'upstream'
    CHECK (origin IN ('upstream', 'writeback'));
ALTER TABLE item_versions ADD COLUMN change_id TEXT;

CREATE TABLE item_changes (
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
    origin TEXT NOT NULL CHECK (origin IN ('dav', 'api')),
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

CREATE INDEX item_changes_item ON item_changes (item_id, id) WHERE item_id IS NOT NULL;
CREATE INDEX item_changes_href ON item_changes (collection_id, href, id) WHERE href IS NOT NULL;
CREATE INDEX item_changes_open ON item_changes (collection_id, state, id)
    WHERE state IN ('held', 'sending', 'conflict', 'rejected');
CREATE INDEX item_changes_archive_open ON item_changes (archive_id, id)
    WHERE state IN ('held', 'sending', 'conflict', 'rejected');
CREATE INDEX item_changes_supersedes ON item_changes (supersedes) WHERE supersedes IS NOT NULL;
CREATE INDEX item_changes_blob ON item_changes (blob_id) WHERE blob_id IS NOT NULL;

CREATE TABLE changes (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    archive_id TEXT NOT NULL,
    collection_id TEXT,
    item_id TEXT,
    href TEXT,
    item_change_id TEXT,
    at INTEGER NOT NULL
);

CREATE INDEX changes_archive ON changes (archive_id, seq);
