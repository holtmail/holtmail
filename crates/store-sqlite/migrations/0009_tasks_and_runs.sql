CREATE TABLE tasks (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    archive_id TEXT,
    account_id TEXT,
    title TEXT NOT NULL,
    state TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    done INTEGER NOT NULL,
    total INTEGER,
    summary TEXT NOT NULL,
    cancellable INTEGER NOT NULL,
    job TEXT,
    log_next INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX tasks_started ON tasks (started_at, id);
CREATE INDEX tasks_state ON tasks (state);

CREATE TABLE task_log (
    task_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    at INTEGER NOT NULL,
    level TEXT NOT NULL,
    text TEXT NOT NULL,
    PRIMARY KEY (task_id, seq)
) WITHOUT ROWID;

CREATE TABLE account_runs (
    account_id TEXT PRIMARY KEY REFERENCES accounts (id),
    finished_at INTEGER NOT NULL,
    new_messages INTEGER NOT NULL,
    error TEXT
);
