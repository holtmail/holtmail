ALTER TABLE collections ADD COLUMN ctag TEXT;
ALTER TABLE collections ADD COLUMN synced_at INTEGER;
ALTER TABLE items ADD COLUMN etag TEXT;
UPDATE items SET etag = (
    SELECT v.etag FROM item_versions v WHERE v.item_id = items.id AND v.number = items.versions
);
