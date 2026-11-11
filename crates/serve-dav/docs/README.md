# holtmail-serve-dav

CalDAV (RFC 4791) and CardDAV (RFC 6352) for the user's own calendar and contacts apps: Apple Calendar and Contacts, DAVx5, Thunderbird, Evolution. A client signs in with a device's app password and sees every calendar and address book of the archives the device covers, the merged "People" book and "From mail". Edits from the client become held changes (`item_changes`) through the same `pim-edit` validation and classification and the same `holtmail-access` locks as the web app; `pim-writeback` sends them upstream for `write_through` collections. A protocol front end, not a contract implementation: it receives the store, the ingest pipeline and `DeviceAuth`, holds nothing of its own, and only `apps/server` mounts it, behind `[serve.dav]`. Design in [SERVE.md](../../../docs/SERVE.md#caldav-and-carddav) and [CLIENTS.md, What is served](../../../docs/CLIENTS.md#what-is-served).

## API

```rust
let dav = Dav::new(store: Arc<dyn Store>, ingest: Ingest, devices: Arc<dyn DeviceAuth>, DavOptions::new(public_url))
    .with_hook(Arc::new(|archive| hub.pim_changed(archive)))   // optional
    .with_scheduler(scheduler: Arc<dyn Scheduler>, hosts: Arc<dyn CalendarHosts>); // optional
let app = web_router
    .layer(axum::middleware::from_fn_with_state(dav.clone(), root_propfind))
    .merge(dav.router());
pub struct DavOptions { public_url, edits, trusted_proxies, switches, scheduling }
#[async_trait] pub trait CalendarHosts { async fn host(&self, &Collection) -> Option<(Host, HostCaps)>; }
```

- `Dav::router()` serves `/dav`, everything below it, `/.well-known/caldav` and `/.well-known/carddav` (301 to `<public_url>/dav/`, for every method, without sign-in). The router needs `ConnectInfo<SocketAddr>` (`into_make_service_with_connect_info`).
- `root_propfind` answers `PROPFIND /` only and passes every other request on, so the web app keeps `/`.
- `ChangeHook` is called with the archive after every recorded change, for live views.
- `with_scheduler` turns on calendar-auto-schedule (see [Scheduling](#scheduling)) while `DavOptions::scheduling` is on. `CalendarHosts::host` says how an account's calendar schedules (`holtmail_schedule::Host`) and what its writer can do there whatever the locks; `None` when its server cannot be asked now, and `Some((Host::Mail, default))` for a collection no server holds. `apps/server` implements it over its collection writers.

## Layout

| Path | What |
| --- | --- |
| `/dav/` (and `PROPFIND /`) | `current-user-principal`, the home sets |
| `/dav/principals/<user>/` | The principal: `calendar-home-set`, `addressbook-home-set`, `calendar-user-address-set` (`mailto:` the sign-in address), `displayname` |
| `/dav/calendars/<user>/<collection>/` | Each calendar of the device's archives, by collection id |
| `/dav/addressbooks/<user>/people-<archive>/` | "People" of each archive, listed first |
| `/dav/addressbooks/<user>/<collection>/` | Each address book |
| `/dav/calendars/<user>/inbox/` | The schedule inbox (only with scheduling): one `<message>-<part>.ics` per incoming iTIP part |
| `/dav/calendars/<user>/outbox/` | The schedule outbox (only with scheduling): free-busy `POST` |
| `<collection>/<name>` | A resource: `items.dav_name`, else the source id. An href holding `/` (an upstream path) is served as `~` plus its base64url form, so clients that decode `%2F` still see one segment; the name never changes |

Another user's path answers 404. Collections removed upstream are not served.

## Guarantees

**Sign-in.** HTTP Basic with an app password, verified on every request as `DeviceProtocol::Dav` (nothing is cached, so a revoked device is refused on its next request). Only when `public_url` is `https`, or from a client on this machine (the address a trusted proxy reports in `X-Forwarded-For`, else the peer); otherwise 403. Wrong or revoked credentials 401 with `WWW-Authenticate: Basic`; a used-up limit 429 with `Retry-After`. Cookies never authenticate. `OPTIONS` needs no sign-in and advertises `1, 3, access-control, calendar-access, addressbook`; with valid credentials it adds `calendar-auto-schedule` (and `POST` to `Allow`) when scheduling is offered to that caller. Without credentials, or with wrong ones, it never does and never answers 401.

**What is served.**

- Every resource the store serves under the collection: live items, items removed upstream with an open change, and held creates (an open change under an href with no item), each once by href. A held delete is not served.
- Bytes: an unedited iCalendar or vCard version is served byte for byte, and its ETag is its blob id (BLAKE3 of the bytes). An item with an open change, or one stored as Graph, JSContact or JSCalendar, is served as `holtmail_pim_sync::Edits::served` gives it (the change re-applied on the latest upstream version, or the deterministic conversion), with BLAKE3 over those bytes as ETag. ETags are strong and quoted.
- vCard 3.0 on request: `address-data version="3.0"` in a report, or `Accept: text/vcard; version=3.0` on `GET`, converts with `calcard`; the converted bytes have their own ETag.
- "People": one vCard 3.0 per person with a card (people from mail alone are left out), `UID` the person id, `FN` the name (else the first address or phone), every address and phone number. Read-only.
- "From mail" (a calendar no account owns): only invitations whose `UID` no other calendar of the same archive holds. Read-only.
- `supported-calendar-component-set`: `VTODO` when the calendar holds tasks, `VEVENT` when it holds events or nothing else ("From mail" always `VEVENT`).
- `max-resource-size`: 2 MiB for calendars, 5 MiB for address books (`pim-edit`'s limits).

**Methods.** `PROPFIND` (Depth 0 and 1; a missing `Depth` is read as 0; `infinity` is 403 `propfind-finite-depth`; `allprop`, `propname` and `prop`; unknown properties 404 in their own `propstat`), `GET`, `HEAD`, `PUT`, `DELETE`, `PROPPATCH`, `REPORT`. `MKCOL`, `MKCALENDAR`, `MOVE` and `COPY` are 403: collections come from accounts.

**Reports.**

| Report | Answer |
| --- | --- |
| `sync-collection` | See Sync |
| `calendar-multiget`, `addressbook-multiget` | Each href of this collection; others and missing ones 404 |
| `calendar-query` | `comp-filter` by component (with `is-not-defined`), `time-range` on `VEVENT`/`VTODO`/`VJOURNAL` through `holtmail-recur` (a recurring event matches when any instance overlaps; an undated task matches any range), `prop-filter` with `text-match` (`contains`, case-insensitive unless `i;octet`, `negate-condition`) and `is-not-defined`. `param-filter`, `time-range` inside a `prop-filter`, partial retrieval and `expand` are ignored: the whole object is returned |
| `addressbook-query` | `prop-filter`s with `test` (`anyof` default, `allof`), `text-match` with `match-type` (`equals`, `contains`, `starts-with`, `ends-with`); `limit/nresults` cuts with a 507 response for the collection |
| `free-busy-query` | A `VFREEBUSY` with merged `FREEBUSY` periods of non-cancelled instances of events not marked `TRANSP:TRANSPARENT` |
| `principal-property-search` | Only the caller's own principal, when every `match` (or any, with `test="anyof"`) is in its name or address. Contacts are not principals |

Anything else is 403 `supported-report`.

**Sync.** Tokens are `urn:holtmail:sync:<collection>:<seq>` from the change log; `getctag` is the same `seq`. For a stored collection the `seq` is the archive's newest item change, so a change in another collection of the archive also moves the ctag (the client then gets an empty `sync-collection`). Without a token the answer lists every resource and the current token. With a token it lists the resources whose `item` rows of this collection came after it: served ones with their properties, others 404, including an item deleted for good (an invitation whose every carrying message was deleted), whose row keeps the href it was served under. More than 1 000 resources are cut with a 507 response for the collection and a token that continues where the answer stopped. A token of another collection, newer than the log or older than its pruned part is 403 `valid-sync-token`. "People" tokens are a digest of every card; any other token than the current one is 403 `valid-sync-token`, so the client lists the book again. For "From mail", an `item` row of another collection of the archive also re-checks the invitations with that item's `UID`, so one hidden or shown because another calendar gained or lost the `UID` is reported by the next token (404 when hidden).

**Writes** ([CLIENTS.md, What is served](../../../docs/CLIENTS.md#what-is-served)).

- A collection is writable when `edits` is on, an account owns it, its write mode is not `read_only`, and `holtmail-access` allows `pim.edit` for the user and the device's scopes (switch, account capability, editor grant, `write:pim`), the same check as the web app's edits. Writable collections advertise `write`, `write-content`, `bind` and `unbind`; every collection advertises `write-properties` (display properties).
- A collection moved from `held` to `read_only` less than 24 hours ago (`Collection::previous_write_mode` and `write_mode_changed_at`) still takes writes as held changes while the other locks are open, without advertising them. Other moves to `read_only` take effect at once.
- Refused writes: 403 `need-privileges`. Wrong media type: 415. Too large: 403 `max-resource-size`. Invalid data, wrong component, a changed `UID`, or a create whose `UID` another item of the collection has: 403 with `pim-edit`'s precondition (`valid-calendar-data`, `valid-address-data`, `supported-calendar-component`, `no-uid-conflict`).
- `PUT` on a free href with `If-None-Match: *` or no precondition: a held create, 201, listed at once (in `PROPFIND`, reports and the next `sync-collection`). On a taken href with `If-None-Match: *`: 412.
- `PUT` on a served resource: a held change (`update`, classified `noop`, `personal`, `reply` or `edit` against the latest upstream version), 204; with `If-Match` the store compares and sets against the served state, so a mismatch with Holtmail's own current ETag is the only 412. A change of a held create stays a change under its href.
- The `ETag` header comes back only while the served bytes equal the client's bytes; after a rebase there is none and clients refetch.
- `DELETE` of a served resource: a held delete, 204; the href then answers 404 and `sync-collection` reports it removed. The archived item and its versions stay.
- Every change records `origin: dav`, the user and the device id, writes an audit entry (`pim_edit`, targets the change id and `device:<id>`), rebuilds the item's derived data and commits the search index, then calls the hook.
- A store or blob failure is 503 with `Retry-After: 5`, the only 5xx.

**Display properties.** `PROPPATCH` on a collection sets or removes `displayname`, `calendar-color`, `calendar-order` (Apple), `calendar-description`, `addressbook-description` and `calendar-timezone`, per user (kept in the settings store under `dav.props.<user>.<collection>`), never sent upstream. All or nothing: one other property makes the request 207 with it 403 and the rest 424, and nothing is stored.

**Logs** carry the device id, the method and the status, never paths, names or content.

## Scheduling

calendar-auto-schedule (RFC 6638) through a `holtmail_schedule::Scheduler`, the same one the web app's RSVP and organizer routes and SMTP submission use ([SERVE.md](../../../docs/SERVE.md#calendar-auto-schedule)).

- **Advertised per caller** only when all hold: `DavOptions::scheduling` and `with_scheduler`; the device has `write:pim` and `send:mail`; in at least one of the device's archives the user passes `holtmail-access`'s sending locks (`may_send_somehow`: switches, a `sender` grant, `send:mail`) and has an active identity. Only these stable facts decide it, so the capability never flaps with a calendar server: a scheduled write to a calendar whose server cannot be asked right now (`CalendarHosts::host` is `None`, also before the server's writers exist) is 503 with `Retry-After`, never recorded without telling the attendees. Otherwise the caller sees no `calendar-auto-schedule`, no scheduling properties, 404 on the inbox and outbox, and writes behave as without scheduling.
- **Principal properties:** `calendar-user-address-set` lists `mailto:` the active identities' addresses and the aliases they cover (without scheduling: the sign-in address), `schedule-inbox-URL`, `schedule-outbox-URL`, `schedule-default-calendar-URL` (the first writable calendar). Calendars answer `schedule-calendar-transp` (`opaque`).
- **`Schedule-Tag`** on `GET`, after `PUT` and as the `schedule-tag` property of calendar resources: the ETag, unless a merged attendee answer pinned the tag it had before the merge (`Item.schedule_tag`, set by the server's iTIP hook); any other recorded change unpins it. `If-Schedule-Tag-Match` on `PUT` and `DELETE` is compared with it: a mismatch, or no resource, is 412; a match records against the state that was served, and a matching `PUT` is prepared with `keep_answers`, so an organizer's client writing from a copy older than a merged answer keeps that answer (RFC 6638 §3.2.10) instead of resetting it.
- **`PUT` of an event:** `Scheduler::prepare` first (client `SCHEDULE-STATUS` and `SCHEDULE-FORCE-SEND` stripped; an attendee's write reduced to what RFC 6638 §3.2.2.1 allows), then `pim-edit` validation, then `Scheduler::deliver` with `Origin::Device`, the host and the abilities open now (the writer's only while the collection writes through and every lock is open), then the change is recorded with the bytes the scheduler returns (`SEQUENCE` raised, `SCHEDULE-STATUS` written) and the held outbox ids linked to it, and `confirm` lets the messages go. When anything after `deliver` fails, `withdraw` takes the messages back; they were held, so nothing ever leaves for a change that was not recorded, whatever the undo window. A stop between recording and `confirm` is caught at the next start, when the outbox releases held messages of recorded changes. The ETag comes back only when the served bytes equal the client's; after a rewrite there is none, so clients refetch.
- **`DELETE` of an event:** delivered as a `CANCEL` (organizer) or a decline (attendee) before the held delete is recorded, withdrawn when recording fails.
- **Schedule inbox:** the archives' `ScheduleStore::inbox_entries` the user has not hidden, newest first. `PROPFIND` (Depth 0 and 1), `GET` (the message's calendar part, byte for byte), `calendar-multiget` and `DELETE` (hides the entry for this user; the archived message stays). ETags are BLAKE3 of message, part and note, so a changed note is a changed resource.
- **Schedule outbox:** `POST` of a `VFREEBUSY` `REQUEST` whose `ORGANIZER` is one of the caller's addresses (else 403 `organizer-allowed`); each own `ATTENDEE` gets `2.0;Success` with merged busy periods of every calendar the caller sees in the requested window, any other `3.7;Invalid calendar user`. Nothing is sent to anyone.
- A scheduler that cannot reach the outbox is 503 with `Retry-After`; unreadable iCalendar is 403 `valid-calendar-data`.

## Decisions

- **No view contract.** The crate reads the store contract directly (collections, `effective`, `open_changes`, the change log, people) through `holtmail-pim-sync`'s `Edits` for served bytes, as SERVE.md lists. Held creates need no local item row: the store already serves an href with an open create (`ChangeTarget::Href`), and `open_changes` lists them; writeback later fills in the item and keeps the href as its DAV name.
- **Grace from the collection.** The 24-hour grace after `held` → `read_only` is read from the collection's own record of its last mode change, so it holds however many audit entries the archive writes and whichever path changed the mode.
- `open_changes` is paged 500 at a time, so every held create is listed however many changes are open.

## Limits

- Listing a collection reads one `effective` per resource, and the bytes only when a property needs them. "People" builds every card on each request.
- No WebDAV-Push yet; clients poll `getctag` or `sync-collection`.
- Every scheduling decision re-reads identities and grants per request; a calendar's host is probed through the server's writers (cached there) when one of its events is written.
- `Schedule-Tag` is offered for every calendar resource once scheduling is on, not only for scheduling objects. A tag pinned by a merged answer stays until the next change through Holtmail or the next upstream version other than the write-back of that answer (the store drops the pin then), so a client holding the old tag gets 412 after an edit made elsewhere.
- `expand-property`, `calendar-proxy`, ACL changes and `MKCALENDAR` are not offered.

## Tests

`cargo test -p holtmail-serve-dav`. Unit tests of the XML reader, paths, tokens, filters, vCard conversion and the People card. `src/tests.rs` runs requests through the router with `tower::ServiceExt::oneshot` against SQLite, a blob directory, a Tantivy index and `device-local`, with invented events and cards on `example.com`/`example.org`: discovery from the well-known address, sign-in rules, byte-for-byte `GET` with the blob hash as ETag, vCard 3.0 on request, multiget, calendar and address book queries, free-busy, principal search, held creates listed at once, 412 only on Holtmail's own ETag, held edits and deletes seen by `sync-collection`, validation refusals, read-only devices and collections, the grace day (only after `held`, unaffected by a busy audit log), display properties, People and From mail (including a delta sync after another calendar takes and gives back a `UID`), an invitation deleted with its mail reported 404 by a delta sync, more than 500 held creates listed, the root `PROPFIND` middleware, Apple, DAVx5 and Thunderbird request shapes (whole answers compared to `tests/golden/*.xml`, with ids replaced by names and one element per line; `HOLTMAIL_BLESS=1 cargo test -p holtmail-serve-dav` rewrites them after a deliberate change), and a loopback test with `collection-dav` reading and writing over a local socket. The `scheduling` tests run `holtmail_schedule::testing::FakeScheduler` over an in-memory submission: advertising only with `send:mail` and only with credentials, the principal properties with alias addresses, an organizer `PUT` delivering a `REQUEST` with `SCHEDULE-STATUS=1.1`, no ETag and a `Schedule-Tag`, 412 on a wrong `If-Schedule-Tag-Match`, a moved event sent again, a `DELETE` sending `CANCEL`, a merged answer keeping the tag, a stale organizer `PUT` with the matching tag keeping the merged answer, a calendar server that cannot be asked giving 503 with nothing sent or recorded while the capability stays advertised, the inbox listing, serving and hiding an entry, and free-busy for own addresses only.
