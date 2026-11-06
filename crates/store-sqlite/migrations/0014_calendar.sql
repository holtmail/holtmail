CREATE TABLE event_series (
    item_id TEXT PRIMARY KEY REFERENCES items (id),
    archive_id TEXT NOT NULL REFERENCES archives (id),
    group_id TEXT NOT NULL,
    sequence INTEGER,
    stamp INTEGER,
    starts_at INTEGER,
    ends_at INTEGER,
    recurring INTEGER NOT NULL,
    window_from INTEGER,
    window_to INTEGER
);
CREATE INDEX event_series_group ON event_series (group_id);
CREATE INDEX event_series_archive ON event_series (archive_id, recurring);

CREATE TABLE occurrences (
    item_id TEXT NOT NULL REFERENCES items (id),
    archive_id TEXT NOT NULL,
    recurrence_id TEXT,
    starts_at INTEGER NOT NULL,
    ends_at INTEGER NOT NULL,
    component INTEGER NOT NULL,
    start_local TEXT NOT NULL,
    end_local TEXT NOT NULL,
    time_zone TEXT,
    all_day INTEGER NOT NULL,
    floating INTEGER NOT NULL,
    cancelled INTEGER NOT NULL,
    changed INTEGER NOT NULL
);
CREATE INDEX occurrences_item ON occurrences (item_id);
CREATE INDEX occurrences_range ON occurrences (archive_id, ends_at, starts_at);

INSERT INTO settings (key, value) VALUES ('index_rebuild', '1')
ON CONFLICT (key) DO UPDATE SET value = excluded.value;
