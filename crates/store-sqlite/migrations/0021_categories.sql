CREATE TABLE category_overrides (
    id TEXT PRIMARY KEY,
    archive_id TEXT NOT NULL,
    scope TEXT NOT NULL CHECK (scope IN ('address', 'domain')),
    key TEXT NOT NULL,
    kind TEXT,
    topics INTEGER,
    created_at INTEGER NOT NULL,
    created_by TEXT NOT NULL,
    UNIQUE (archive_id, scope, key)
);

CREATE TABLE message_categories (
    message_id TEXT PRIMARY KEY REFERENCES messages (id),
    archive_id TEXT NOT NULL,
    derive_fp TEXT NOT NULL,
    trust TEXT NOT NULL CHECK (trust IN ('aligned', 'relayed', 'unverified', 'unknown')),
    trust_domain TEXT,
    trust_authserv TEXT,
    one_click_ok INTEGER NOT NULL,
    own INTEGER NOT NULL,
    sender_address TEXT,
    sender_domain TEXT,
    derived_kind TEXT NOT NULL,
    derived_kind_source TEXT NOT NULL,
    derived_topics INTEGER NOT NULL,
    derived_sources TEXT NOT NULL,
    kind TEXT NOT NULL,
    list_topic TEXT,
    override_ids TEXT
) WITHOUT ROWID;
CREATE INDEX message_categories_fp ON message_categories (archive_id, derive_fp);
CREATE INDEX message_categories_kind ON message_categories (archive_id, kind, message_id);
CREATE INDEX message_categories_address ON message_categories (archive_id, sender_address);
CREATE INDEX message_categories_domain ON message_categories (archive_id, sender_domain);

CREATE TABLE message_topics (
    message_id TEXT NOT NULL REFERENCES messages (id),
    archive_id TEXT NOT NULL,
    topic TEXT NOT NULL,
    PRIMARY KEY (message_id, topic)
) WITHOUT ROWID;
CREATE INDEX message_topics_filter ON message_topics (archive_id, topic, message_id);

CREATE TABLE account_authserv (
    account_id TEXT PRIMARY KEY REFERENCES accounts (id),
    ids TEXT NOT NULL
) WITHOUT ROWID;
