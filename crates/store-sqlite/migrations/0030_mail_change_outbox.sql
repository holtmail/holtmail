CREATE INDEX mail_changes_outbox ON mail_changes (outbox_id, id) WHERE outbox_id IS NOT NULL;
