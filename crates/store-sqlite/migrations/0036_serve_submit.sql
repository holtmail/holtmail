ALTER TABLE transports ADD COLUMN smtputf8 INTEGER NOT NULL DEFAULT 0;

CREATE INDEX outbox_device_sends ON outbox (user_id, created_at) WHERE device IS NOT NULL;
