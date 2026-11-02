CREATE TABLE archives (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE accounts (
    id TEXT PRIMARY KEY,
    archive_id TEXT NOT NULL REFERENCES archives (id),
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    address TEXT,
    settings TEXT NOT NULL,
    sealed_secret BLOB,
    enabled INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX accounts_archive ON accounts (archive_id, id);

CREATE TABLE folders (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts (id),
    path TEXT NOT NULL,
    role TEXT,
    created_at INTEGER NOT NULL,
    UNIQUE (account_id, path)
);

CREATE TABLE messages (
    id TEXT PRIMARY KEY,
    archive_id TEXT NOT NULL REFERENCES archives (id),
    thread_id TEXT NOT NULL,
    blob_id TEXT NOT NULL,
    size INTEGER NOT NULL,
    rfc_message_id TEXT,
    subject TEXT,
    from_json TEXT NOT NULL,
    to_json TEXT NOT NULL,
    cc_json TEXT NOT NULL,
    bcc_json TEXT NOT NULL,
    sent_at INTEGER,
    received_at INTEGER,
    archived_at INTEGER NOT NULL,
    date INTEGER NOT NULL,
    preview TEXT NOT NULL,
    attachment_count INTEGER NOT NULL,
    UNIQUE (archive_id, blob_id)
);
CREATE INDEX messages_list ON messages (archive_id, date DESC, id DESC);
CREATE INDEX messages_thread ON messages (thread_id, date, id);
CREATE INDEX messages_rfc ON messages (archive_id, rfc_message_id);

CREATE TABLE message_parents (
    message_id TEXT NOT NULL REFERENCES messages (id),
    archive_id TEXT NOT NULL,
    parent_rfc_id TEXT NOT NULL,
    PRIMARY KEY (message_id, parent_rfc_id)
);
CREATE INDEX message_parents_rfc ON message_parents (archive_id, parent_rfc_id);

CREATE TABLE attachments (
    message_id TEXT NOT NULL REFERENCES messages (id),
    ordinal INTEGER NOT NULL,
    blob_id TEXT NOT NULL,
    filename TEXT,
    content_type TEXT NOT NULL,
    size INTEGER NOT NULL,
    content_id TEXT,
    inline INTEGER NOT NULL,
    PRIMARY KEY (message_id, ordinal)
);
CREATE INDEX attachments_blob ON attachments (blob_id);

CREATE TABLE locations (
    folder_id TEXT NOT NULL REFERENCES folders (id),
    message_id TEXT NOT NULL REFERENCES messages (id),
    uid INTEGER,
    flags TEXT NOT NULL,
    first_seen_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    removed_at INTEGER,
    PRIMARY KEY (folder_id, message_id)
);
CREATE UNIQUE INDEX locations_uid ON locations (folder_id, uid) WHERE uid IS NOT NULL;
CREATE INDEX locations_message ON locations (message_id);

CREATE TABLE folder_cursors (
    folder_id TEXT PRIMARY KEY REFERENCES folders (id),
    uid_validity INTEGER,
    last_uid INTEGER,
    highest_modseq INTEGER,
    state TEXT,
    synced_at INTEGER
);

CREATE TABLE sync_failures (
    folder_id TEXT NOT NULL REFERENCES folders (id),
    uid INTEGER NOT NULL,
    reason TEXT NOT NULL,
    attempts INTEGER NOT NULL,
    failed_at INTEGER NOT NULL,
    PRIMARY KEY (folder_id, uid)
);
