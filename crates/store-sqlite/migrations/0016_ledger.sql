CREATE TABLE ledger (
    seq INTEGER PRIMARY KEY,
    at INTEGER NOT NULL,
    event TEXT NOT NULL,
    archive_id TEXT NOT NULL,
    message_id TEXT,
    blob_id TEXT,
    actor TEXT,
    prev TEXT NOT NULL,
    hash TEXT NOT NULL
);
CREATE INDEX ledger_message ON ledger (message_id, event) WHERE message_id IS NOT NULL;
CREATE INDEX ledger_archive ON ledger (archive_id, event, seq);
