# holtmail-collection

The contract for reading an account's address books and calendars: CardDAV and CalDAV ([`collection-dav`](../../collection-dav/docs/README.md)), Microsoft Graph ([`collection-graph`](../../collection-graph/docs/README.md)) and JMAP ([`collection-jmap`](../../collection-jmap/docs/README.md)). The [sync engine](../../pim-sync/docs/README.md) only talks to this contract. A source is read-only for address books and calendars: it never creates, changes or deletes an item or a collection. The only writes are push registrations it is asked for (`subscribe`, `unsubscribe`). Writing items is the separate [`CollectionWriter`](#writing-collectionwriter) contract.

## Trait

```rust
#[async_trait]
pub trait CollectionSource: Send + Sync {
    async fn collections(&self) -> Result<Vec<RemoteCollection>, CollectionError>;
    async fn changes(&self, collection: &str, token: Option<&str>) -> Result<Changes, CollectionError>;
    async fn fetch(&self, collection: &str, ids: &[String]) -> Result<Vec<RemoteItem>, CollectionError>;
    async fn list(&self, collection: &str) -> Result<Vec<ItemTag>, CollectionError>;
    async fn watch(&self) -> Result<Option<Box<dyn CollectionWatch>>, CollectionError> { Ok(None) }
    async fn subscribe(&self, collection: &str, subscription: &PushSubscription)
        -> Result<Option<PushRegistration>, CollectionError> { Ok(None) }
    async fn unsubscribe(&self, registration: &PushRegistration) -> Result<(), CollectionError> { Ok(()) }
}
```

| Item | Guarantees |
| --- | --- |
| `collections()` | Every address book and calendar, each once, in the server's order; scheduling inboxes and outboxes left out |
| `RemoteCollection { id, kind, name, color, components, tag }` | `id` is stable across listings (a DAV path); `kind` is `Contacts` or `Calendar`; `name` falls back to a name made from the id; `color` as written; `components` lists `VEVENT`/`VTODO`… in upper case for calendars, empty when unknown; `tag` (a `getctag` or `sync-token`) changes whenever anything inside changes, `None` when the server keeps none |
| `changes(collection, None)` | Every item now in the collection as changed, nothing removed, and a first token |
| `changes(collection, Some(token))` | Exactly the items added or changed and the ids removed since `token`, each once (an item changed then removed is listed once, as removed; `removed` may also name ids that were never in the collection, which callers ignore), and the next token; asking again with that token lists only later changes. `TokenExpired` when the server no longer accepts the token, `Unsupported` when the collection cannot list changes |
| `fetch(collection, ids)` | The bytes exactly as the server sent them, with the ETag and content type (one of `media`, the same for every item of a collection), in any order; ids no longer there are left out; an id asked twice is answered once |
| `list(collection)` | Every item now in the collection with its ETag, sorted by id, without bytes: the recovery listing |
| `ItemTag { id, etag }` | `id` is stable in its collection; `etag` changes whenever the bytes change |
| `watch()` | A watch on a connection of its own (a JMAP event source); `Ok(None)` (the default) when the server cannot push that way |
| `CollectionWatch::next()` | Returns once something may have changed since the watch started or the last return; `CollectionsChanged { collections }` names the collection ids, or `None` for every collection. A change made while the watch is alive is never swallowed; returns without a change are allowed; an error ends the watch; dropping the future gives the watch up |
| `subscribe(collection, subscription)` | Asks the server to post Web Push notices (RFC 8030, encrypted for the subscription's keys as RFC 8291 says) to `subscription.endpoint` whenever the collection changes. `Ok(None)` (the default) when the collection offers no such push, without asking. A new registration for the same endpoint replaces the old one. Notices are hints and stop once the registration expired |
| `PushSubscription { endpoint, public_key, auth_secret, expires }` | The https address, the P-256 public key (65 bytes, uncompressed), the 16-byte auth secret and the end the subscriber asks for. `Debug` hides the auth secret |
| `PushRegistration { url, expires }` | The registration's own address on the server and when the server stops sending to it, if it said |
| `unsubscribe(registration)` | Ends the registration; one the server no longer knows is not an error |

## Media types

`media` names the formats an item may come in, and `media::kind_of(content_type)` says which kind of collection holds it (parameters and case ignored):

| Constant | Media type | Kind |
| --- | --- | --- |
| `VCARD` | `text/vcard` | contacts |
| `ICALENDAR` | `text/calendar` | calendar |
| `GRAPH_CONTACT` | `application/vnd.microsoft.graph.contact+json` | contacts |
| `GRAPH_EVENT` | `application/vnd.microsoft.graph.event+json` | calendar |
| `JSCONTACT` | `application/jscontact+json` (RFC 9553) | contacts |
| `JSCALENDAR` | `application/jscalendar+json` (RFC 8984) | calendar |

A source never converts: the bytes are what its server sent, and [`holtmail-pim`](../../pim/docs/README.md) reads each type (`Format::from_content_type`).

## Credentials

```rust
pub enum Credentials {
    Password { username: String, password: String },
    Bearer(String),
}
```

The contract has its own credentials rather than reusing `holtmail_source::Credentials`, so neither contract depends on the other. `Debug` prints only the variant. A bearer token expires, so the server builds a source with a fresh one before each sync.

## Errors

| Error | Meaning | Retry |
| --- | --- | --- |
| `Auth` | Credentials rejected | No, until they change |
| `Connection` | Unreachable, broke off, or busy after waiting | Yes |
| `Protocol` | Unexpected answer, or a redirect to a server credentials may not go to | Yes (`is_transient`) |
| `Config` | Bad address, or no address book or calendar there | No |
| `TokenExpired` | The server forgot the token; `changes(None)` or `list` work | Start over |
| `Unsupported` | No changes since a token for this collection; `list` works | Use `list` |
| `Gone` | The collection is not on the server any more | No |
| `PreconditionFailed { current }` | A write's precondition did not hold (the item changed, exists or is gone); `current` is the server's tag now, `None` when the item is gone or the server keeps no tags | Not as it is: record upstream, rebase, then write again |
| `Refused` | The server will not take the write (read-only, invalid, too large, a field it cannot set) | No |

Messages carry host names and status codes only, never names, contents or secrets.

## For an implementation

Pass the conformance suite: depend on `holtmail-collection` with `features = ["testing"]` in `[dev-dependencies]` and call

```rust
holtmail_collection::testing::conformance(&source, &remote).await;
```

where `remote` implements `testing::Remote` (`put`, `delete`, `expire_tokens`, and optionally `stored`) against the same server, which starts with exactly one empty address book and one empty calendar. The suite stores contacts with CRLF line ends, `&`, `<` and UTF-8 in them and an event, checks listings, fetches byte for byte, one id asked twice, tags that change with the bytes, collection tags that change with the content, removed ids left out of fetches, changes since a token (changed, added and removed), a quiet second token, and `TokenExpired` after `expire_tokens` with `changes(None)` still working. A source answering `Unsupported` skips the token part.

A source with a watch also runs `testing::watch_conformance(&source, &remote).await` against a server with at least one address book: a new card wakes the watch and names the address book (or every collection), and a change and a removal after it wake it again. Each item's media type must belong to its collection's kind and be the same for every item of the collection. The suite puts vCards and iCalendar objects; a server whose format cannot carry them as they are (JMAP's JSON) wraps them in its item object, and `Remote::stored(collection, name, raw)` returns the bytes the source then fetches (by default `raw` itself).

## Writing: `CollectionWriter`

Writes are a second trait, separate from `CollectionSource`, which stays read-only ([CLIENTS.md, Write-through](../../../docs/CLIENTS.md#write-through-writemd-phase-61)). Only `apps/server` will hand one out, and only for collections in `write_through` mode on accounts with `pim.edit` (and `pim.respond` for `respond`); `pim-writeback` drains held changes through it. All three implementations (`collection-dav`, `collection-jmap`, `collection-graph`) are writers; the server builds the writer of a collection from the protocol its kind is read with, so `collection-merge` needs no write routing.

```rust
#[async_trait]
pub trait CollectionWriter: Send + Sync {
    fn capabilities(&self, collection: &str) -> WriteCaps;
    async fn put(&self, collection: &str, id: Option<&str>, change: &ItemPatch,
                 precondition: Precondition, idempotency: &str) -> Result<Written, CollectionError>;
    async fn delete(&self, collection: &str, id: &str, if_match: Option<&str>) -> Result<(), CollectionError>;
    async fn respond(&self, collection: &str, id: &str, recurrence_id: Option<&str>,
                     answer: Partstat, notify_organizer: bool) -> Result<Written, CollectionError>;
}
```

| Item | Guarantees |
| --- | --- |
| `capabilities(collection)` | `WriteCaps { create, update, delete, respond, field_patch, tags, max_size, keeps_uid }`; all off (`WriteCaps::default()`) for a collection the writer knows is read-only or does not exist (a writer that has not listed its collections yet may answer from the id alone). `keeps_uid`: a create keeps `ItemPatch::uid` (off for Graph, which assigns its own; the item read back then carries the server's). `field_patch`: updates send only the patch's fields (Graph, JMAP); `tags`: `IfMatch` works, otherwise callers use `IfUnchanged` |
| `put(collection, None, patch, _, key)` | Creates an item, conditional on it not existing, idempotent by `key`: a repeat after success answers with the same item and creates no second one (a deterministic href with `If-None-Match: *` on DAV, `transactionId` on Graph, a lookup by `UID` on JMAP) |
| `put(collection, Some(id), patch, precondition, _)` | Changes the item under `Precondition::{None, Absent, IfMatch(tag), IfUnchanged(bytes)}`; a precondition that does not hold, or a missing item, is `PreconditionFailed { current }` and changes nothing |
| `delete(collection, id, if_match)` | Removes the item; one already gone succeeds; a different tag under `if_match` is `PreconditionFailed` |
| `respond(collection, id, recurrence_id, answer, notify_organizer)` | Sets only the account's own `PARTSTAT` (`Partstat::{NeedsAction, Accepted, Tentative, Declined}`) for the series or one occurrence, and asks the server to tell the organizer when asked; a missing item is `PreconditionFailed { current: None }` |
| `Written { id, etag, item }` | The item's id as the source lists it, its tag now, and the item as the server holds it when the answer carried it (otherwise the caller fetches it for the writeback version) |

A call failing with `PreconditionFailed`, `Refused`, `Auth` or `Gone` changed nothing. Any other failure may or may not have reached the server, and repeating the call is safe.

**`ItemPatch { content_type, raw, uid, fields }`** is what `pim-edit` computes for a change: `raw` is the whole item after the change as iCalendar or vCard (`media::ICALENDAR`, `media::VCARD`), and `fields` lists the parts that differ from the upstream base (`Field::{Summary, Start, End, Recurrence, Location, Description, Status, Organizer, Attendees, Alarms, Categories, Transparency, Privacy, Name, Emails, Phones, Addresses, Organization, Title, Notes, Dates, Photo, Urls, Property(name)}`), `None` for every part. Replace-only writers (CalDAV, CardDAV) send `raw` unchanged and ignore `fields`. Field-patching writers (Graph, JMAP) read the listed fields out of `raw` and send only those, so alarms, categories and photos the client never saw survive; a field they cannot set is `Refused` before anything changes. One representation for every writer keeps `pim-edit` free of provider formats; the converters live in the writers.

**Conformance.** `testing::writer_conformance(&writer, &probe, collection, kind).await` runs against an empty writable collection, with `probe` implementing `testing::WriteProbe` (`own_address`, `read`, `ids`, `touch` to change an item behind the writer's back, `partstat`, `holds` to compare what the server stores with what was written, byte equality by default, `uid` to read the `UID` out of what it stores, the first `UID` line by default, and `unknown_collection`, an id of the writer's shape naming no collection, `/no/such/collection/` by default). It checks that an unknown collection offers nothing, creates and their idempotency by key, the created item's `UID` when `keeps_uid`, the tag in the answer, an update under a holding precondition, a stale precondition failing with the current tag and changing nothing, an upstream change failing the precondition, the last writer winning without one, conditional and repeated deletes, an update of a missing item, `Gone` for an unknown collection, and, when the writer can respond, answers for the series and one occurrence.

## Testing

`testing::FakeCollections` is an in-memory server: `add_collection`, `remove_collection`, `put_item`, `delete_item`, `set_media` (answer a collection's items with another media type), `expire`, `without_changes`, `reject_sign_in`, and `fetches`/`listings` to see what a sync asked for. `enable_watch`, `break_watches` and `watches` give it a watch that reports the collections every put, delete and removal touches; `enable_push` makes every collection accept subscriptions, and `registrations` lists them. Clones share one server. It passes the conformance suite with and without changes, and the watch suite.

The fake is also a `CollectionWriter` and its own `WriteProbe`: `enable_writes` turns writing on (until then every collection offers nothing and writes are `Refused`), `refuse_writes(reason)` refuses every write, `reject_sign_in` fails writes with `Auth`, and `notices` lists the answers sent to organizers. Created items are named `<key>.ics` or `<key>.vcf`; writes are seen by the source (tags, changes, watches). It passes `writer_conformance` for a calendar and an address book.
