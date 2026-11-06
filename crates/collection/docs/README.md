# holtmail-collection

The contract for reading an account's address books and calendars: CardDAV and CalDAV ([`collection-dav`](../../collection-dav/docs/README.md)), Microsoft Graph ([`collection-graph`](../../collection-graph/docs/README.md)) and JMAP ([`collection-jmap`](../../collection-jmap/docs/README.md)). The [sync engine](../../pim-sync/docs/README.md) only talks to this contract. A source is read-only: it never creates, changes or deletes anything on the server.

## Trait

```rust
#[async_trait]
pub trait CollectionSource: Send + Sync {
    async fn collections(&self) -> Result<Vec<RemoteCollection>, CollectionError>;
    async fn changes(&self, collection: &str, token: Option<&str>) -> Result<Changes, CollectionError>;
    async fn fetch(&self, collection: &str, ids: &[String]) -> Result<Vec<RemoteItem>, CollectionError>;
    async fn list(&self, collection: &str) -> Result<Vec<ItemTag>, CollectionError>;
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

Messages carry host names and status codes only, never names, contents or secrets.

## For an implementation

Pass the conformance suite: depend on `holtmail-collection` with `features = ["testing"]` in `[dev-dependencies]` and call

```rust
holtmail_collection::testing::conformance(&source, &remote).await;
```

where `remote` implements `testing::Remote` (`put`, `delete`, `expire_tokens`, and optionally `stored`) against the same server, which starts with exactly one empty address book and one empty calendar. The suite stores contacts with CRLF line ends, `&`, `<` and UTF-8 in them and an event, checks listings, fetches byte for byte, one id asked twice, tags that change with the bytes, collection tags that change with the content, removed ids left out of fetches, changes since a token (changed, added and removed), a quiet second token, and `TokenExpired` after `expire_tokens` with `changes(None)` still working. A source answering `Unsupported` skips the token part. Each item's media type must belong to its collection's kind and be the same for every item of the collection. The suite puts vCards and iCalendar objects; a server whose format cannot carry them as they are (JMAP's JSON) wraps them in its item object, and `Remote::stored(collection, name, raw)` returns the bytes the source then fetches (by default `raw` itself).

## Testing

`testing::FakeCollections` is an in-memory server: `add_collection`, `remove_collection`, `put_item`, `delete_item`, `set_media` (answer a collection's items with another media type), `expire`, `without_changes`, `reject_sign_in`, and `fetches`/`listings` to see what a sync asked for. It passes the conformance suite with and without changes.
