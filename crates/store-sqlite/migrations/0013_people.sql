CREATE TABLE message_addresses (
    message_id TEXT NOT NULL REFERENCES messages (id),
    archive_id TEXT NOT NULL,
    field TEXT NOT NULL,
    email TEXT NOT NULL,
    name TEXT,
    date INTEGER NOT NULL,
    PRIMARY KEY (message_id, field, email)
) WITHOUT ROWID;
CREATE INDEX message_addresses_email ON message_addresses (archive_id, email);

-- trim() with Rust's whitespace (Unicode White_Space), as the insert path trims.
INSERT OR IGNORE INTO message_addresses (message_id, archive_id, field, email, name, date)
SELECT m.id, m.archive_id, f.field, lower(trim(json_extract(j.value, '$.email'), char(9, 10, 11, 12, 13, 32, 133, 160, 5760, 8192, 8193, 8194, 8195, 8196, 8197, 8198, 8199, 8200, 8201, 8202, 8232, 8233, 8239, 8287, 12288))),
       NULLIF(trim(json_extract(j.value, '$.name'), char(9, 10, 11, 12, 13, 32, 133, 160, 5760, 8192, 8193, 8194, 8195, 8196, 8197, 8198, 8199, 8200, 8201, 8202, 8232, 8233, 8239, 8287, 12288)), ''), m.date
FROM messages m
JOIN (SELECT 'from' AS field UNION ALL SELECT 'to' UNION ALL SELECT 'cc' UNION ALL SELECT 'bcc') f
JOIN json_each(CASE f.field WHEN 'from' THEN m.from_json WHEN 'to' THEN m.to_json
                            WHEN 'cc' THEN m.cc_json ELSE m.bcc_json END) j
WHERE instr(trim(json_extract(j.value, '$.email'), char(9, 10, 11, 12, 13, 32, 133, 160, 5760, 8192, 8193, 8194, 8195, 8196, 8197, 8198, 8199, 8200, 8201, 8202, 8232, 8233, 8239, 8287, 12288)), '@') > 1;

CREATE TABLE people (
    id TEXT PRIMARY KEY,
    archive_id TEXT NOT NULL REFERENCES archives (id),
    name_key TEXT NOT NULL,
    search TEXT NOT NULL,
    cards INTEGER NOT NULL,
    differences INTEGER NOT NULL,
    data TEXT NOT NULL
);
CREATE INDEX people_list ON people (archive_id, name_key, id);

CREATE TABLE person_emails (
    archive_id TEXT NOT NULL,
    email TEXT NOT NULL,
    person_id TEXT NOT NULL REFERENCES people (id),
    PRIMARY KEY (archive_id, email)
) WITHOUT ROWID;

CREATE TABLE person_collections (
    person_id TEXT NOT NULL REFERENCES people (id),
    collection_id TEXT NOT NULL,
    PRIMARY KEY (person_id, collection_id)
) WITHOUT ROWID;
CREATE INDEX person_collections_collection ON person_collections (collection_id);
