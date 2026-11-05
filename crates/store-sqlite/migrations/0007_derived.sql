CREATE TABLE derived (
    source_blob TEXT NOT NULL,
    kind TEXT NOT NULL,
    blob_id TEXT,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (source_blob, kind)
);
CREATE INDEX derived_blob ON derived (blob_id);
