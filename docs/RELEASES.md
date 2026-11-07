# Release notes

Changes people notice when they upgrade: new behavior, changed meaning and one-time work after the upgrade.

## Unreleased

### New search operators

- `has:code`, `has:link`, `has:unsubscribe`, `has:reservation`, `has:order`, `has:invoice`, `has:parcel` and `has:pass` find messages with that kind of [insight](INSIGHTS.md) at `Likely` or higher, outside forwarded parts.
- `category:<topic>` (`purchases`, `travel`, `finance`, `health`, `social`, `jobs`, `events`, `government`) and `category:<kind>` (`personal`, `list`, `bulk`, `notification`) find messages by their effective category. Any other `category:` value is an error that lists the allowed ones.
- `is:newsletter` (kind `bulk`), `is:notification`, `is:personal` and `is:list` find messages of that kind.

Values are never searchable: `has:code` finds messages that contain a code, not the code itself.

### Changed meaning

`has:code`, `is:newsletter`, `category:travel` and the other values above used to be searched as plain text (the words `has` and `code`, anywhere). They now select by insights and categories, so the same query finds other messages. A deletion review typed with one of them shows the new selection in its count and sample before anything is deleted, and is refused with 409 "Some messages are still being read" while the archive's insights or search index are still catching up. With insights turned off, such a review is refused with 400. Unknown `has:` and `is:` values are still searched as plain text.

### One-click unsubscribe

- When a newsletter's sender signed its `List-Unsubscribe` and `List-Unsubscribe-Post` fields with an aligned DKIM signature that a trusted server checked, the insights card offers Unsubscribe. After you confirm, the server sends one RFC 8058 POST to the sender's https link, directly and never through `image_proxy`, so the sender sees the server's public address. Owners and admins only; members still see no unsubscribe links.
- A request accepted or redirected in the last 24 hours is not sent again for the same list; after a failure, a new try is allowed after a minute. Every request is recorded, and the card shows "Unsubscribe requested on …".
- New config key `[insights] one_click` (default `true`). Set it to `false` to turn one-click off: no request is ever sent and the card offers only the sender's link.

### Category data updates

- New config key `[insights.categories] update` (default `false`). Set it to `true` to check weekly for a signed release of the domain category data on the project's GitHub release `categories-latest`. The server connects directly, never through `image_proxy`, reaches only the GitHub release hosts, and loads a release only if its ed25519 signature, key, expiry and file hashes check out and its version is newer. It needs `domains` in `[insights.categories] sources`.
- New config key `[insights.categories] data_sources` (default `["wikidata"]`) names the files to download. Add `"ut1"` to also use the UT1-derived file (shopping, jobs and social sites, CC BY-SA 4.0); it is never bundled. The bundled Wikidata table always loads.
- Accepted releases are stored in `{data_dir}/categories/` and loaded at start; with `update = false`, deleting the folder goes back to the bundled table. A downloaded table is used only while it is newer than the one bundled with the running version.
- A newly loaded release re-derives categories in the background without reading any message.
- New route `GET /api/insights/data` and a new Settings → About page for every signed-in user: the Holtmail version, the loaded data files with versions, licenses and attribution, and the update state.
- No release signing key ships yet, so with `update = true` every release is refused (`no_release_key` in the log) until one is compiled in; nothing else changes.

### After the upgrade

The search index gains two fields, so the first start after the upgrade deletes it and rebuilds it from the archive once. Search results are incomplete until the rebuild finishes; the rebuild also fills any missing insights.

Migration `0023_unsubscribe.sql` adds the empty `unsubscribe_requests` table on the first start; nothing is backfilled. Deleting a message keeps its recorded requests with the message link cleared.

Category data updates add no migration; four settings rows (`categories_data_*`) appear after the first check, and the `categories` folder only after the first accepted release.
