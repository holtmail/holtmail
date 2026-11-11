CREATE TABLE served_mailboxes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    archive_id TEXT NOT NULL REFERENCES archives (id) ON DELETE CASCADE,
    user_id TEXT REFERENCES users (id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    uidvalidity INTEGER NOT NULL CHECK (uidvalidity BETWEEN 1 AND 4294967295),
    uidnext INTEGER NOT NULL CHECK (uidnext >= 1),
    highest_modseq INTEGER NOT NULL,
    synced_seq INTEGER NOT NULL,
    lowest_modseq INTEGER NOT NULL,
    built INTEGER NOT NULL CHECK (built IN (0, 1)),
    stale INTEGER NOT NULL DEFAULT 0 CHECK (stale IN (0, 1)),
    build_ord INTEGER,
    build_member TEXT,
    build_total INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX served_mailboxes_key ON served_mailboxes (archive_id, key, IFNULL(user_id, ''));

CREATE TABLE served_uids (
    mailbox_id INTEGER NOT NULL REFERENCES served_mailboxes (id) ON DELETE CASCADE,
    uid INTEGER NOT NULL,
    member TEXT NOT NULL,
    modseq INTEGER NOT NULL,
    expunged_modseq INTEGER,
    PRIMARY KEY (mailbox_id, uid)
) WITHOUT ROWID;
CREATE UNIQUE INDEX served_uids_member ON served_uids (mailbox_id, member)
    WHERE expunged_modseq IS NULL;
CREATE INDEX served_uids_modseq ON served_uids (mailbox_id, modseq)
    WHERE expunged_modseq IS NULL;
CREATE INDEX served_uids_tombstones ON served_uids (mailbox_id, expunged_modseq)
    WHERE expunged_modseq IS NOT NULL;

CREATE INDEX messages_served ON messages (archive_id, archived_at, id);
