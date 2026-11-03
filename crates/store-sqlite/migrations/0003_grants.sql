CREATE TABLE archive_grants (
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    archive_id TEXT NOT NULL REFERENCES archives (id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, archive_id)
);
CREATE INDEX archive_grants_archive ON archive_grants (archive_id);
