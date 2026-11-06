CREATE TABLE deleted_blobs (
    archive_id TEXT NOT NULL,
    blob_id TEXT NOT NULL,
    deleted_at INTEGER NOT NULL,
    PRIMARY KEY (archive_id, blob_id)
) WITHOUT ROWID;

CREATE TABLE deleted_locations (
    folder_id TEXT NOT NULL,
    uid INTEGER NOT NULL,
    deleted_at INTEGER NOT NULL,
    PRIMARY KEY (folder_id, uid)
) WITHOUT ROWID;
