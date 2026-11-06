CREATE TABLE message_aliases (
    message_id TEXT PRIMARY KEY REFERENCES messages (id),
    archive_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    alias TEXT NOT NULL,
    sender_email TEXT,
    sender_name TEXT,
    reverse_alias TEXT,
    signed INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX message_aliases_alias ON message_aliases (archive_id, alias);
CREATE INDEX message_aliases_reverse ON message_aliases (archive_id, reverse_alias)
    WHERE reverse_alias IS NOT NULL;

CREATE TABLE alias_services (
    id TEXT PRIMARY KEY,
    archive_id TEXT NOT NULL REFERENCES archives (id),
    provider TEXT NOT NULL,
    name TEXT NOT NULL,
    base_url TEXT NOT NULL,
    sealed_key BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    synced_at INTEGER,
    sync_error TEXT
);
CREATE INDEX alias_services_archive ON alias_services (archive_id, created_at);

CREATE TABLE aliases (
    archive_id TEXT NOT NULL,
    address TEXT NOT NULL,
    provider TEXT NOT NULL,
    service_id TEXT REFERENCES alias_services (id),
    remote_id TEXT,
    enabled INTEGER,
    note TEXT,
    name TEXT,
    for_domain TEXT,
    mailboxes TEXT NOT NULL DEFAULT '[]',
    created_at INTEGER,
    forwarded INTEGER,
    blocked INTEGER,
    replied INTEGER,
    PRIMARY KEY (archive_id, address)
) WITHOUT ROWID;
CREATE INDEX aliases_service ON aliases (service_id) WHERE service_id IS NOT NULL;

-- Addresses of mail with each reverse alias replaced by the sender it stands for, for people.
CREATE VIEW resolved_addresses AS
SELECT a.message_id, a.archive_id, a.field,
    COALESCE(c.sender_email, a.email) AS email,
    CASE WHEN c.sender_email IS NULL THEN a.name ELSE c.sender_name END AS name,
    a.date
FROM message_addresses a
LEFT JOIN (
    SELECT archive_id, reverse_alias, MAX(sender_email) AS sender_email,
        MAX(sender_name) AS sender_name
    FROM message_aliases
    WHERE reverse_alias IS NOT NULL AND sender_email IS NOT NULL
    GROUP BY archive_id, reverse_alias
) c ON c.archive_id = a.archive_id AND c.reverse_alias = a.email;
