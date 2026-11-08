CREATE TABLE mail_changes (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts (id),
    folder_id TEXT NOT NULL REFERENCES folders (id),
    message_id TEXT,
    uid INTEGER,
    kind TEXT NOT NULL CHECK (kind IN ('flags', 'labels', 'move', 'delete', 'append')),
    payload TEXT NOT NULL,
    blob_id TEXT,
    origin TEXT NOT NULL CHECK (origin IN ('user', 'rule', 'retention', 'sent_copy')),
    user_id TEXT,
    outbox_id TEXT,
    created_at INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('held', 'sending', 'applied', 'superseded_by_server',
                                         'failed')),
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER,
    last_error TEXT,
    sent_at INTEGER,
    result_uid INTEGER,
    updated_at INTEGER NOT NULL
);

CREATE INDEX mail_changes_account ON mail_changes (account_id, id);
CREATE INDEX mail_changes_open ON mail_changes (account_id, state, id)
    WHERE state IN ('held', 'sending');
CREATE INDEX mail_changes_message ON mail_changes (message_id, id) WHERE message_id IS NOT NULL;
CREATE INDEX mail_changes_location ON mail_changes (folder_id, uid)
    WHERE state IN ('held', 'sending');
CREATE INDEX mail_changes_blob ON mail_changes (blob_id) WHERE blob_id IS NOT NULL;
