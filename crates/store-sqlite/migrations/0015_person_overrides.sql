CREATE TABLE person_overrides (
    id TEXT PRIMARY KEY,
    archive_id TEXT NOT NULL REFERENCES archives (id),
    kind TEXT NOT NULL,
    keys TEXT NOT NULL,
    from_keys TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    created_by TEXT
);
CREATE INDEX person_overrides_archive ON person_overrides (archive_id, id);
