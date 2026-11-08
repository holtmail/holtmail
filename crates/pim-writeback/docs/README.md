# holtmail-pim-writeback

Sends held edits of contacts and events to their servers ([CLIENTS.md, Write-through](../../../docs/CLIENTS.md#write-through-writemd-phase-61), [WRITE.md, Contacts and calendars](../../../docs/WRITE.md#contacts-and-calendars)). An edit from a device or the web app is a row in the store's `item_changes` from the moment it is made. For collections in `write_through` mode, `Writeback` takes the rows first in first out per collection, hands each to the account's `CollectionWriter` and settles it: applied, a conflict, rejected, or held again after a temporary failure. A change is never dropped: whatever happens, it stays in the store, and a conflict or refusal stays served until someone decides. A library over `holtmail-store` (`ItemChangeStore`, `PimStore`), `holtmail-blob`, `holtmail-collection` (`CollectionWriter`, `CollectionSource`), `holtmail-ingest` (recording upstream versions, rebuilding derived data) and `holtmail-pim` (reading a version's kind and `UID`). The composition root supplies `Writers` (the gate) and `Rebaser` (from `pim-edit`).

## Public API

### `Writers` (the caller implements it)

`upstream(&Collection) -> Result<Option<Upstream { writer, source }>, CollectionError>`: the account's writer and the source that reads its items back, when the collection's changes may be sent now. `None` means writing is closed for now (the account's `pim.edit` switch or the `write:pim` scope is off). The change then waits with `not_allowed` and the account pauses, as after a temporary failure. An error counts like one from the writer.

`may_reply(&Collection) -> Result<bool, CollectionError>` (default `true`): whether `reply` changes (only the account's own answer changed) may go upstream now, which the server answers from the account's `pim.respond`. A `put` of an attendee copy with a new answer makes an auto-scheduling CalDAV server tell the organizer (RFC 6638), so a reply is an answer to the invitation. While it is `false`, reply changes wait, held with `respond_off`, and the account's other changes go on.

`may_edit(&Collection) -> Result<bool, CollectionError>` (default `true`): whether changes other than replies may go upstream now, which the server answers from the account's `pim.edit`. While it is `false` they wait, held with `edit_off`, and the account's replies go on, so an account with only `pim.respond` answers invitations without its other edits leaving.

`pim_write_consent_missing(&Account)`: a Microsoft Graph account signed in with OAuth whose last sign-in did not grant `Contacts.ReadWrite` and `Calendars.ReadWrite` (`oauth.pim_write`); the server's gate gives such an account no writer.

### `Rebaser` (the caller implements it, from `pim-edit`)

Pure, no I/O. It is injected so that this crate never reads patches or provider formats.

- `fields(&ItemChange) -> Option<Vec<Field>>` returns the parts the change touched, read from its stored patch. It goes into `ItemPatch::fields`, so field-patching writers (Graph, JMAP) send only these parts. The default `None` means every part.
- `rebased(&ItemChange, mine, onto, onto_type) -> Option<Bytes>` gives the whole item to send for a change the post-batch step rebased (`rebased_onto` differs from `base_version`): the change re-applied on that upstream version (raw as stored, of media type `onto_type`), so a replace-only writer does not overwrite upstream's own later edits. The default `None` sends the change's own bytes.
- `rebase_reply(&ItemChange, mine, &RemoteItem) -> Option<Bytes>` applies the account's own answer in `mine` (the change's whole item) on top of the item as upstream holds it now. It returns `None` when the answer no longer fits (cancelled, occurrence gone, no longer invited).

- `respond(&ItemChange) -> Option<Respond { recurrence_id, answer, notify }>` (default `None`): for a `reply` change answered through the RSVP API, how it goes through `CollectionWriter::respond`. Where the writer's `WriteCaps::respond` is on, the change is sent with `respond` instead of `put` (the server keeps the rest of the event and other attendees' answers), telling the organizer only with `notify`; elsewhere it is a `put` like any reply.

`NoRebase` knows nothing: every part counts as changed and no reply is rebased.

### `Writeback`

`Writeback::new(store, blobs, ingest, writers, rebaser, WritebackConfig)`; `with_clock(f)` swaps the clock for tests.

`WritebackConfig` defaults:

- `first_retry`: 30 s, doubling per attempt up to `max_retry`.
- `max_retry`: 24 h. A change is retried at that pace for as long as it takes, never failed because of time.
- `poll`: 2 s.
- `batch`: 50 changes per collection per pass.

| Method | Guarantees |
| --- | --- |
| `drain_collection(collection)` | Claims the collection's due changes one at a time (`claim_change`) in the order they were made, at most `batch`, settles each (`settle_change`) and returns how many it attempted. The store's claim guarantees first in first out per collection, one change per item in flight, only the head of a supersede chain, never a `noop` or `personal` change, and never one flagged by an upstream change. Nothing is sent while the account is paused |
| `drain_due()` | `drain_collection` for every collection with a due change whose account is not paused: accounts in parallel, the collections of one account one after another |
| `recover()` | Puts changes left `sending` by a stopped process back to `held`, or to `superseded` when a later change took their place, and returns the held ones' ids. Call once at start, before `run`. Sending them again is safe: creates are idempotent by key, and an update that already landed fails its precondition and becomes a conflict |
| `run()` | `drain_due` in a loop, woken by `wake()` or every `poll`; runs until the process ends |
| `wake()` | Wakes `run` now; call it after recording a change |

### What one attempt does

| Change | Writer call | Settled as |
| --- | --- | --- |
| No item yet (a held create, or an update or delete that superseded it) | `put(collection, None, patch, Absent, key)` with `key` the change's href (else its id), so every change of the chain creates the same item | Applied with the new item; the store names it by the href (`dav_name`). A taken name is a conflict `exists` |
| Delete with no item | none | Applied: nothing reached upstream |
| Update of an item | `put(collection, Some(remote_id), patch, precondition, change id)` | Applied, or after a failed precondition see below |
| A `reply` with `Rebaser::respond`, where the writer offers `respond` | `respond(collection, remote_id, recurrence_id, answer, notify)` | Applied with what the server answered (or the item fetched back; the sent bytes never vouch for a tag); a failed precondition is a conflict `upstream_changed`, or `upstream_deleted` when the item is gone |
| Delete of an item | `delete(collection, remote_id, etag)` | Applied (the item is marked removed); after a failed precondition, a conflict, or applied when the item is gone upstream anyway |

**The patch** carries the change's stored bytes (or, for a rebased change, `Rebaser::rebased` on the version it was rebased onto) and media type, the item's `UID` (its key, else read from the bytes) and `Rebaser::fields`. A writer that cannot do the operation (`WriteCaps`) rejects it as `unsupported`. Bytes over `WriteCaps::max_size` are rejected as `too_large`.

**The precondition** comes from the upstream version the change is based on: `rebased_onto`, else `base_version`, else the latest. It is `IfMatch` with that version's tag, else `IfUnchanged` with its bytes, else `None`.

**On success**, what upstream now holds is recorded as the item's newest version with origin `writeback` and the change's id. The user's own later changes of the item, which superseded the one sent while it was in flight, are rebased onto that version by the store rather than flagged, so they are sent next. The item's calendar entries and search document are then rebuilt from what is served: the open head change re-applied on the new version with `Rebaser::rebased` when there is one, else the new version. "What upstream now holds" is the writer's answer (`Written::item`), else the item read back with `fetch`, else the bytes that were sent. Those sent bytes carry `Written::etag` only for a writer that replaces whole items. After a field patch, no tag is kept, so the next sync fetches the real item. When storing the version fails, or settling fails for any other reason, the change is still applied, without a version, and the next sync brings the item; a failed settle of any other outcome holds the change again with `store`, so it is never left `sending`.

**On a failed precondition** (412), the item as upstream holds it now is fetched and recorded as an upstream version through `Ingest::ingest_item`, which flags the item's open changes. When the item is gone, it is marked removed instead. Then:

- A `reply` is rebased with `Rebaser::rebase_reply` and sent once more, under upstream's new tag. If it fails its precondition again, it becomes a conflict. If the rebaser returns `None`, it becomes a conflict `reply_obsolete`.
- Any other change becomes a conflict `upstream_changed`, or `upstream_deleted` when the item is gone.
- When the fetch itself fails, the change is held again like any temporary failure.

### Outcome codes

Rejections and conflicts set `reason`; retries set `last_error` and `next_attempt_at`.

| Error | Outcome | Pauses the account |
| --- | --- | --- |
| `Auth` | held, `auth` | yes |
| `Connection` | held, `connection` | yes |
| `Config` | held, `config` | yes |
| writing closed (`Writers` answers `None`) | held, `not_allowed` | yes |
| `Protocol`, `TokenExpired` | held, `protocol` | no |
| a `reply` while `Writers::may_reply` is `false` | held, `respond_off` | no |
| any other change while `Writers::may_edit` is `false` | held, `edit_off` | no |
| store or blob store failures | held, `store` / `blob` | no |
| `Refused` | rejected, `refused` | no |
| `Unsupported`, or not in `WriteCaps` | rejected, `unsupported` | no |
| `Gone` (the collection is gone upstream) | rejected, `collection_gone` | no |
| change bytes missing from the blob store | rejected, `missing_bytes` | no |
| a change without bytes, or naming an unknown item | rejected, `invalid` | no |

A held change waits `first_retry · 2^(attempts−1)`, capped at `max_retry` (24 h). When the account pauses, the pass stops there and all of the account's collections wait out the same backoff. The pause doubles per failed pass and is kept in memory. Any other outcome ends the pause, so a bad password costs one sign-in per backoff step, never one per held change.

Logs carry change, collection, item and account ids, codes and counts. They never carry item contents, names, addresses or server text.

## Limits

- Replies are sent as a `put` of the whole item unless `Rebaser::respond` names a `respond`. iMIP replies through the outbox are the API's, not this crate's.
- Refusals and conflicts are recorded on the change and logged. No user notification is sent from here yet.
- The pause is per process and in memory. After a restart, a paused account is tried once at once.
- One process drains: `run` must not be started twice over the same store.

## Tests

`src/tests.rs` runs the worker over the SQLite store, the file blob store, an in-memory Tantivy index and `FakeCollections` from `holtmail-collection`'s `testing` feature as writer and source. The tests cover:

- an edit recorded as a `writeback` version with upstream's tag;
- a create that is idempotent by href and named by it;
- a delete;
- a failed precondition recording upstream and becoming a conflict;
- an item gone upstream;
- a rebased reply, and an obsolete one;
- a change rebased by the post-batch step sending its rebase;
- a refusal;
- an account paused after a rejected sign-in;
- network backoff capped at 24 h without ever dropping the change;
- a closed gate;
- a reply through `respond` telling the organizer once, and one after an iMIP answer telling nobody;
- edits waiting with `edit_off` while replies go on;
- only the head of a supersede chain being sent;
- `recover`.

```
cargo test -p holtmail-pim-writeback
```
