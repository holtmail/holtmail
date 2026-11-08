CREATE TABLE reverse_aliases (
    archive_id TEXT NOT NULL,
    reverse_alias TEXT NOT NULL,
    alias TEXT NOT NULL,
    contact TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (archive_id, reverse_alias)
) WITHOUT ROWID;

DROP VIEW resolved_addresses;

-- A pair seen in mail wins over one recorded when a reverse alias was made.
CREATE VIEW resolved_addresses AS
SELECT a.message_id, a.archive_id, a.field,
    COALESCE(c.sender_email, a.email) AS email,
    CASE WHEN c.sender_email IS NULL THEN a.name ELSE c.sender_name END AS name,
    a.date
FROM message_addresses a
LEFT JOIN (
    SELECT archive_id, reverse_alias, MAX(sender_email) AS sender_email,
        MAX(sender_name) AS sender_name
    FROM message_aliases
    WHERE reverse_alias IS NOT NULL AND sender_email IS NOT NULL
    GROUP BY archive_id, reverse_alias
    UNION ALL
    SELECT r.archive_id, r.reverse_alias, r.contact, NULL
    FROM reverse_aliases r
    WHERE NOT EXISTS (
        SELECT 1 FROM message_aliases m
        WHERE m.archive_id = r.archive_id AND m.reverse_alias = r.reverse_alias
            AND m.sender_email IS NOT NULL)
) c ON c.archive_id = a.archive_id AND c.reverse_alias = a.email;
