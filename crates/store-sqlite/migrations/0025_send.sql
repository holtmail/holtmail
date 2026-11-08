CREATE TABLE transports (
    id TEXT PRIMARY KEY,
    archive_id TEXT REFERENCES archives (id),
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('smtp', 'account')),
    account_id TEXT REFERENCES accounts (id),
    settings TEXT NOT NULL,
    sealed_secret BLOB,
    allowed_from TEXT NOT NULL,
    max_size INTEGER,
    max_recipients INTEGER,
    per_hour INTEGER,
    enabled INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX transports_archive ON transports (archive_id);

CREATE TABLE transport_grants (
    transport_id TEXT NOT NULL REFERENCES transports (id) ON DELETE CASCADE,
    archive_id TEXT NOT NULL REFERENCES archives (id),
    PRIMARY KEY (transport_id, archive_id)
) WITHOUT ROWID;
CREATE INDEX transport_grants_archive ON transport_grants (archive_id);

CREATE TABLE identities (
    id TEXT PRIMARY KEY,
    archive_id TEXT NOT NULL REFERENCES archives (id),
    address TEXT NOT NULL,
    name TEXT,
    signature_html TEXT,
    signature_text TEXT,
    reply_to TEXT,
    transport_id TEXT NOT NULL REFERENCES transports (id),
    sent_copy TEXT NOT NULL,
    bcc_self INTEGER NOT NULL,
    account_id TEXT REFERENCES accounts (id),
    alias TEXT,
    plus_addresses INTEGER NOT NULL,
    active INTEGER NOT NULL,
    created_by TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX identities_archive ON identities (archive_id);
CREATE INDEX identities_transport ON identities (transport_id);

CREATE TABLE identity_users (
    identity_id TEXT NOT NULL REFERENCES identities (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    PRIMARY KEY (identity_id, user_id)
) WITHOUT ROWID;
CREATE INDEX identity_users_user ON identity_users (user_id);

CREATE TABLE identity_defaults (
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    archive_id TEXT NOT NULL REFERENCES archives (id),
    identity_id TEXT NOT NULL REFERENCES identities (id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, archive_id)
) WITHOUT ROWID;

CREATE TABLE drafts (
    id TEXT PRIMARY KEY,
    archive_id TEXT NOT NULL REFERENCES archives (id),
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    version INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    saved_at INTEGER NOT NULL
);
CREATE INDEX drafts_owner ON drafts (archive_id, user_id, saved_at);

CREATE TABLE draft_versions (
    draft_id TEXT NOT NULL REFERENCES drafts (id) ON DELETE CASCADE,
    version INTEGER NOT NULL,
    payload TEXT NOT NULL,
    blobs TEXT NOT NULL,
    saved_at INTEGER NOT NULL,
    PRIMARY KEY (draft_id, version)
) WITHOUT ROWID;

CREATE TABLE draft_blobs (
    draft_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    blob_id TEXT NOT NULL,
    PRIMARY KEY (draft_id, version, blob_id),
    FOREIGN KEY (draft_id, version) REFERENCES draft_versions (draft_id, version) ON DELETE CASCADE
) WITHOUT ROWID;
CREATE INDEX draft_blobs_blob ON draft_blobs (blob_id);

CREATE TABLE outbox (
    id TEXT PRIMARY KEY,
    archive_id TEXT NOT NULL REFERENCES archives (id),
    identity_id TEXT NOT NULL,
    transport_id TEXT NOT NULL,
    user_id TEXT,
    route TEXT NOT NULL,
    mail_from TEXT NOT NULL,
    recipients TEXT NOT NULL,
    blob_id TEXT NOT NULL,
    size INTEGER NOT NULL,
    rfc_message_id TEXT NOT NULL,
    send_at INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('draft', 'queued', 'sending', 'retrying', 'sent',
                                         'failed', 'uncertain', 'bounced', 'cancelled')),
    queued_state TEXT NOT NULL CHECK (queued_state IN ('draft', 'queued')),
    flow_run TEXT,
    idempotency_key TEXT,
    resend_of TEXT,
    created_at INTEGER NOT NULL,
    message_id TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_attempt_at INTEGER,
    next_attempt_at INTEGER,
    last_error TEXT,
    sent_at INTEGER,
    updated_at INTEGER NOT NULL
);
CREATE INDEX outbox_archive ON outbox (archive_id, id);
CREATE INDEX outbox_due ON outbox (transport_id, state);
CREATE INDEX outbox_blob ON outbox (blob_id);
CREATE INDEX outbox_rfc_message_id ON outbox (archive_id, rfc_message_id);
CREATE UNIQUE INDEX outbox_idempotency ON outbox (user_id, idempotency_key)
    WHERE user_id IS NOT NULL AND idempotency_key IS NOT NULL;
