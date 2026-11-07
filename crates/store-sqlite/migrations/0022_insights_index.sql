CREATE TABLE insight_reindex (
    message_id TEXT PRIMARY KEY REFERENCES messages (id),
    archive_id TEXT NOT NULL,
    seq INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX insight_reindex_archive ON insight_reindex (archive_id, message_id);
