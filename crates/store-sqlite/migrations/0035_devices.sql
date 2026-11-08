CREATE TABLE devices (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    mail_archive TEXT REFERENCES archives (id) ON DELETE SET NULL,
    all_archives INTEGER NOT NULL,
    protocols TEXT NOT NULL,
    scopes TEXT NOT NULL,
    implicit_seen INTEGER NOT NULL DEFAULT 0,
    secret_hash BLOB UNIQUE,
    created_at INTEGER NOT NULL,
    last_used_day INTEGER,
    last_protocol TEXT,
    revoked_at INTEGER
);
CREATE INDEX devices_user ON devices (user_id, id);

CREATE TABLE device_archives (
    device_id TEXT NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
    archive_id TEXT NOT NULL REFERENCES archives (id) ON DELETE CASCADE,
    PRIMARY KEY (device_id, archive_id)
) WITHOUT ROWID;
CREATE INDEX device_archives_archive ON device_archives (archive_id);

ALTER TABLE mail_changes ADD COLUMN device TEXT;
ALTER TABLE outbox ADD COLUMN device TEXT;
