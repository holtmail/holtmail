ALTER TABLE collections ADD COLUMN write_mode_changed_at INTEGER;
ALTER TABLE collections ADD COLUMN previous_write_mode TEXT;
