ALTER TABLE mail_changes ADD COLUMN dismissed_at INTEGER;

CREATE INDEX mail_changes_failed ON mail_changes (message_id, id)
    WHERE state = 'failed' AND dismissed_at IS NULL AND message_id IS NOT NULL;
