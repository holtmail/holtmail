# holtmail-pim-sync

Archives an account's address books and calendars with their history, through a [`CollectionSource`](../../collection/docs/README.md) and the [ingest pipeline](../../ingest/docs/README.md) (`Ingest::ingest_item`), so a version from a server is stored exactly like one imported from a file.

## Usage

```rust
let syncer = PimSyncer::new(store, ingest);
let report = syncer.sync(archive_id, account_id, &source).await?;
```

`PimReport { collections, removed_collections }` with one `CollectionReport { collection, kind, created, mode, new, changed, same, removed, failed, error }` per collection the server listed; `new_items()`, `changed_items()`, `removed_items()` and `error()` sum them up. `mode` says how changes were read: `Unchanged` (the collection's tag had not changed), `SinceToken`, `Full` (first sync, or a collection back after removal), `TokenExpired` (the stored token was refused) or `Listing` (the server cannot list changes).

## Guarantees

- **Collections.** Each listed collection is stored on first sight (`upsert_collection` with the account) and renamed or recolored as the server says. A stored collection of the account the server no longer lists is marked removed with its items; one that comes back is a collection again and is read in full.
- **Versions.** Only items whose ETag differs from the stored one (or that have none) are fetched, 50 at a time (`FETCH_BATCH`). A version is added only when the bytes changed; the same bytes under a new ETag only update the item's current ETag. Calendar objects are kept whole, one item per resource, whatever components they hold; their kind (event, task, other) comes from the first component. Each item is stored in the format its media type names (`holtmail_pim::Format::from_content_type`: vCard, iCalendar, Graph JSON, JSContact, JSCalendar), as that version's content type; a type that does not fit the collection's kind, or none at all, is read as vCard for address books and iCalendar for calendars.
- **Removals.** Since a token, the ids the server reports removed are marked removed. In a full read, stored items missing from the listing are. Nothing is ever deleted.
- **Recovery.** A refused token reads every item's ETag again (`changes` without a token) and diffs; a server without changes (`Unsupported`) is listed every time (`list`) and keeps no token.
- **Tokens.** A collection's token and tag are stored (`set_collection_synced`) only after its changes were fully applied; when an item could not be stored, or the server listed an item but `fetch` left it out (a server that cannot send it), the item counts as `failed` and both are cleared so the next run reads everything again and asks for it once more. A collection whose sync fails reports its `error`, keeps its old token, and the others go on.
- **Idempotent.** Running again with nothing changed on the server fetches nothing and changes nothing; with a tag the server keeps, nothing but the collection list is asked.
- **Search.** Each stored version also updates the item's calendar instances and search document (`Ingest::ingest_item`); the search index is committed once at the end of `sync`, and a failed commit is logged, not reported, since the index is derived.
- **Errors.** Failing to list collections (a refused sign-in, an unreachable server) is `PimSyncError::Source` before anything is stored. Logs carry ids and counts only.
