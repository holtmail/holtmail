CREATE TABLE alias_strategies (
    archive_id TEXT NOT NULL,
    scope TEXT NOT NULL CHECK (scope IN ('alias', 'domain', 'service')),
    key TEXT NOT NULL,
    strategy TEXT NOT NULL,
    PRIMARY KEY (archive_id, scope, key)
) WITHOUT ROWID;
