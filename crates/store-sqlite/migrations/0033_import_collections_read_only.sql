UPDATE collections SET write_mode = 'read_only'
WHERE account_id IN (SELECT id FROM accounts WHERE kind = 'import');
