CREATE TABLE flows (
    id TEXT PRIMARY KEY,
    archive_id TEXT NOT NULL REFERENCES archives (id),
    name TEXT NOT NULL,
    position INTEGER NOT NULL,
    mode TEXT NOT NULL CHECK (mode IN ('test', 'on', 'off')),
    paused_reason TEXT,
    paused_at INTEGER,
    version INTEGER NOT NULL,
    trigger TEXT NOT NULL,
    saved_by TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX flows_archive ON flows (archive_id, position);

CREATE TABLE flow_versions (
    flow_id TEXT NOT NULL REFERENCES flows (id),
    version INTEGER NOT NULL,
    definition TEXT NOT NULL,
    saved_by TEXT NOT NULL,
    saved_at INTEGER NOT NULL,
    PRIMARY KEY (flow_id, version)
);

CREATE TABLE flow_runs (
    id TEXT PRIMARY KEY,
    flow_id TEXT NOT NULL REFERENCES flows (id),
    archive_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    event_id TEXT NOT NULL,
    trigger TEXT NOT NULL,
    test INTEGER NOT NULL,
    message_id TEXT,
    sender TEXT,
    started_at INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('running', 'done', 'failed', 'interrupted')),
    plan TEXT,
    results TEXT NOT NULL DEFAULT '[]',
    error TEXT,
    acted INTEGER NOT NULL DEFAULT 0,
    finished_at INTEGER,
    UNIQUE (flow_id, version, event_id)
);
CREATE INDEX flow_runs_flow ON flow_runs (flow_id, id);
CREATE INDEX flow_runs_started ON flow_runs (flow_id, started_at);
CREATE INDEX flow_runs_running ON flow_runs (status) WHERE status = 'running';

CREATE TABLE flow_actions (
    run_id TEXT NOT NULL,
    flow_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    identity_id TEXT,
    recipient TEXT,
    at INTEGER NOT NULL
);
CREATE INDEX flow_actions_flow ON flow_actions (flow_id, at);
CREATE INDEX flow_actions_identity ON flow_actions (identity_id, at) WHERE identity_id IS NOT NULL;
CREATE INDEX flow_actions_reply ON flow_actions (flow_id, recipient, at) WHERE kind = 'reply';

CREATE TABLE rule_events (
    archive_id TEXT NOT NULL,
    id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    trigger TEXT NOT NULL,
    message_id TEXT,
    item_id TEXT,
    live INTEGER NOT NULL,
    payload TEXT NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY (archive_id, id)
);
CREATE INDEX rule_events_due ON rule_events (seq);
