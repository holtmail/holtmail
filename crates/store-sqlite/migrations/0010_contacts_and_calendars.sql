CREATE TABLE collections (
    id TEXT PRIMARY KEY,
    archive_id TEXT NOT NULL REFERENCES archives (id),
    account_id TEXT REFERENCES accounts (id),
    kind TEXT NOT NULL,
    remote_id TEXT NOT NULL,
    name TEXT NOT NULL,
    color TEXT,
    sync_token TEXT,
    created_at INTEGER NOT NULL,
    removed_at INTEGER
);
CREATE INDEX collections_archive ON collections (archive_id, remote_id);
CREATE INDEX collections_account ON collections (account_id);

CREATE TABLE items (
    id TEXT PRIMARY KEY,
    collection_id TEXT NOT NULL REFERENCES collections (id),
    archive_id TEXT NOT NULL REFERENCES archives (id),
    remote_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    key TEXT,
    first_seen_at INTEGER NOT NULL,
    removed_at INTEGER,
    versions INTEGER NOT NULL,
    UNIQUE (collection_id, remote_id)
);
CREATE INDEX items_archive ON items (archive_id, kind);
CREATE INDEX items_key ON items (archive_id, key);

CREATE TABLE item_versions (
    item_id TEXT NOT NULL REFERENCES items (id),
    number INTEGER NOT NULL,
    blob_id TEXT NOT NULL,
    content_type TEXT NOT NULL,
    etag TEXT,
    seen_at INTEGER NOT NULL,
    photo_blob_id TEXT,
    PRIMARY KEY (item_id, number)
) WITHOUT ROWID;
CREATE INDEX item_versions_blob ON item_versions (blob_id);
CREATE INDEX item_versions_photo ON item_versions (photo_blob_id) WHERE photo_blob_id IS NOT NULL;

CREATE TABLE pim_index (
    item_id TEXT PRIMARY KEY REFERENCES items (id),
    version INTEGER NOT NULL,
    readable INTEGER NOT NULL,
    title TEXT,
    title_key TEXT,
    starts_at INTEGER,
    ends_at INTEGER,
    all_day INTEGER NOT NULL,
    time_zone TEXT,
    recurring INTEGER NOT NULL,
    sequence INTEGER,
    uid TEXT,
    organizer TEXT,
    emails TEXT NOT NULL,
    phones TEXT NOT NULL,
    data TEXT NOT NULL
);
CREATE INDEX pim_index_title ON pim_index (title_key, item_id);
CREATE INDEX pim_index_start ON pim_index (starts_at, item_id);
