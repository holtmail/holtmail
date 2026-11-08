# holtmail-collection-dav

Reads address books and calendars from CardDAV (RFC 6352) and CalDAV (RFC 4791) servers behind the [`CollectionSource`](../../collection/docs/README.md) contract, and writes to them behind [`CollectionWriter`](../../collection/docs/README.md#writing-collectionwriter). Reading uses `PROPFIND`, `REPORT`, `OPTIONS` and `GET` only; nothing is written unless the server hands the `DavSource` out as a writer (see [Writing](#writing)). WebDAV-Push adds a `POST` registering a push subscription with a collection that offers it, and a `DELETE` ending it.

## Config

```rust
let source = DavSource::new(DavConfig {
    contacts: Some("https://carddav.fastmail.com".into()),
    calendars: Some("https://caldav.fastmail.com".into()),
    credentials: Credentials::Password { username, password },
    timeout: Duration::from_secs(60),
})?;
// or DavConfig::at("https://cloud.example.com/remote.php/dav", credentials) for both
```

`with_addresses(addresses)` names the account's own addresses for answering invitations, besides the `calendar-user-address-set` the server lists for the signed-in principal.

`with_roots(&TlsRoots)` trusts the given CA certificates (from Holtmail's `tls_ca_file`, see [`holtmail-core`](../../core/docs/README.md)) besides the system's roots; verification and the host name check stay on. An unusable certificate is a `Config` error.

Each address may be a server (its well-known address is asked), a DAV root, a principal, a home set or a single address book or calendar. `None` reads no contacts or no calendars. `https://` is assumed without a scheme; plain `http` is refused except to loopback addresses, and so are addresses with a user name or password in them.

| Provider | Contacts | Calendars | Sign-in |
| --- | --- | --- | --- |
| Fastmail | `https://carddav.fastmail.com` | `https://caldav.fastmail.com` | app password |
| iCloud | `https://contacts.icloud.com` | `https://caldav.icloud.com` | app-specific password |
| Yandex | `https://carddav.yandex.ru` | `https://caldav.yandex.ru` | app password |
| Google | `https://www.googleapis.com/.well-known/carddav` | `https://apidata.googleusercontent.com/caldav/v2/` | OAuth bearer token |
| Nextcloud | `https://host/remote.php/dav` | the same | password or app password |
| Radicale, Baïkal, others | the server, or its DAV root | the same | password |

## Discovery

1. An address without a path, or a `/.well-known/` one, is looked up at `/.well-known/carddav` or `/.well-known/caldav` (RFC 6764) **without credentials**, following redirects (never from https to plain http); where it lands becomes the starting point. A `/.well-known/` address whose lookup leads nowhere is refused with `Config` before any credentials are sent; an address without a path is then asked itself.
2. `PROPFIND` depth 0 asks for `current-user-principal`, the home set and `resourcetype`. A home set is used directly; an address book or calendar itself is the only collection; otherwise the principal is asked for its home set; otherwise the address is treated as a home set.
3. `PROPFIND` depth 1 on each home set lists collections with `displayname`, `getctag` (any namespace), `sync-token`, `calendar-color` (any namespace), `supported-calendar-component-set` and `max-resource-size`. Only `addressbook` and `calendar` resource types count.
4. For calendars, one `OPTIONS` per home set (or single calendar) reads the `DAV` header: `calendar-auto-schedule` there (RFC 6638) means the server sends scheduling messages itself. A failed `OPTIONS` counts as no.

Collection and item ids are the percent-decoded paths, so servers that encode the same path differently from one answer to the next (`@` and `%40`) give one id; requests encode them again. The collections found are remembered for the calls that follow; an unknown id lists them again, then answers `Gone`.

## Changes and bodies

- `changes` sends `sync-collection` (RFC 6578, `Depth: 0`, `sync-level 1`, `getetag`). A `404` in a response is a removal. A `507` for the collection itself means the server cut the list: the next page is asked with the new token, up to 100 pages and a million changes; a cut list whose new token equals the one asked with is `Protocol`, since it would never end. A refusal of a given token (`valid-sync-token`, or `400`/`403`/`409`/`412`) is `TokenExpired`; a refusal without a token (`403`, `405`, `501`, …) is `Unsupported`.
- `list` is `PROPFIND` depth 1 with `getetag`, leaving out the collection and sub-collections.
- `fetch` sends `addressbook-multiget` or `calendar-multiget` with up to 50 hrefs (`BATCH`). A server that refuses multiget (`400`, `403`, `405`, `415`, `422`, `501`) is read with one `GET` per item from then on. `404` answers are left out. A multiget answered with a server error (`5xx`) is asked again one `GET` per item for that batch only; an item whose `GET` fails too (a `5xx` or no answer) is left out and logged as a count, and the others are returned. sabre/dav (Baïkal, Nextcloud) parses every card of a multiget and answers the whole request with `500` when one of them does not parse, so one broken card would otherwise stop the whole address book; [`holtmail-pim-sync`](../../pim-sync/docs/README.md) counts the left-out item as failed and asks again on the next run.
- `address-data` and `calendar-data` are kept exactly: the XML text is decoded without line-end normalization, so CRLF (raw or as `&#13;`) survives byte for byte.

## Writing

`DavSource` implements `CollectionWriter`. Capabilities come from the last collection listing: a collection not listed yet offers nothing until `collections` (or any write, which lists again for an unknown id) has run, so the server keeps one `DavSource` per account for reading and writing.

| Capability | Value |
| --- | --- |
| `create`, `update`, `delete`, `tags` | `true` in every listed collection |
| `field_patch` | `false`: the whole item is replaced |
| `max_size` | the collection's `max-resource-size`; a larger item is `Refused` before anything is sent |
| `respond` | only in calendars whose server advertises `calendar-auto-schedule`; elsewhere replies take the iMIP route |

- **Create:** `PUT` with `If-None-Match: *` to a name made from the idempotency key (letters, digits, `-` and `.` kept, any other byte written as `_` and two hex digits, then `.ics` or `.vcf`), so a repeat goes to the same href. A `412` there reads the item: when it carries the change's `UID` (its bytes, without one) it is the earlier create and the answer is success; otherwise `PreconditionFailed` with its tag.
- **Update:** `PUT` with `If-Match`: the given tag for `IfMatch`, `*` for `None` (a missing item is not made again), the tag read just before for `IfUnchanged` after comparing bytes; `Absent` sends `If-None-Match: *`. An id outside the collection, `404` or `410` is `PreconditionFailed` without a tag.
- **Delete:** `DELETE` with `If-Match` when given; `404`, `410`, and a `412` for an item gone meanwhile, count as deleted.
- **Content type:** only `text/calendar` in calendars and `text/vcard` in address books, sent with `charset=utf-8`; anything else is `Refused`.
- **Answers:** a `PUT` of the attendee copy, which RFC 6638 §3.2.2.1 lets an attendee change and an auto-scheduling server turns into a reply to the organizer. The event is read, the account's own `ATTENDEE` lines (matched without `mailto:`, ignoring case) get the new `PARTSTAT` and lose `SCHEDULE-STATUS`, and every other line keeps its bytes; a changed line is folded at 75 octets. Without a `recurrence_id` the master and every overridden occurrence are answered; with one, its override is answered, or a new override is added after the last event, copied from the master without `RRULE`, `RDATE`, `EXDATE`, `EXRULE`, `DTEND` and `DURATION`, with `RECURRENCE-ID` and `DTSTART` taking the master's `DTSTART` parameters (no `TZID` for a UTC id, `VALUE=DATE` for a date) and a `DURATION` from the master's length. The `PUT` carries `If-Match` with the tag read; a `412` reads again, three tries in all. `notify_organizer` unset sends `Schedule-Reply: F` (RFC 6638 §8.1); set, it is `Refused` in a calendar whose server does not schedule and for an event whose organizer or own attendee has `SCHEDULE-AGENT` other than `SERVER`. An item with none of the account's addresses as attendee is `Refused`.
- **Answers to errors:** `412` is `PreconditionFailed` with the current tag (read with `GET`, else `PROPFIND` `getetag`); `401` is `Auth`; `403`, `405`, `409`, `413`, `415`, `422` and `507` are `Refused`, naming the status and the `DAV:error` precondition (`valid-calendar-data`, `no-uid-conflict`, …); `404` on a create is `Gone`.
- **Written:** a `PUT` answered with a strong `ETag` returns it with the item as sent (RFC 4791 §5.3.4: a server sends a strong `ETag` only for unchanged bytes); a weak `W/` tag is returned with `item` `None`, since the server may have rewritten the item; without a tag it is read with `PROPFIND` and `item` is `None`. Callers fetch what the server kept whenever `item` is `None`. Servers that rewrite an item yet answer with a strong tag break the RFC; [VERIFY.md](../../../docs/VERIFY.md) lists the check against Radicale and Stalwart.
- **Logs** carry only whether a write succeeded.

## WebDAV-Push

The [WebDAV-Push draft](https://bitfireat.github.io/webdav-push/) (namespace `https://bitfire.at/webdav-push`, as published on 29 October 2026; it still changes) lets a server post a notice to a Web Push address when a collection changes.

- **Discovery.** The collection listing also asks for `transports` and `supported-triggers`. A collection whose `transports` holds `web-push` offers push; the `content-update` depth it lists is used, `1` when it lists none. A `vapid-public-key` is not used: Holtmail receives the notices itself and does not check VAPID.
- **`subscribe`** sends `POST` to the collection with `push-register`: the subscription's `push-resource`, `content-encoding` `aes128gcm`, `subscription-public-key` (`type="p256dh"`, base64url) and `auth-secret`, a `content-update` trigger, and `expires` as an HTTP date when the subscription has one. `201` or `204` with a `Location` is a registration, with the `Expires` header as its end when sent. A collection that does not list `web-push` is answered `None` without a request; a `403` naming `push-not-available` or `no-supported-trigger` (or the schema's `no-trigger-supported`), or a `405` or `501`, is `None` too; any other `403` is `Protocol`. A `Location` on a host credentials may not go to is refused with `Protocol`, since `unsubscribe` signs in there.
- **`unsubscribe`** sends `DELETE` to the registration; `404` and `410` count as ended.
- Notices are decrypted by whoever owns the push address ([`holtmail-webpush`](../../webpush/docs/README.md) in the server); this crate never sees them.

## Limits and safety

- **Credentials stay home.** They are sent only over https (http only to loopback), to the configured servers, to where a well-known lookup of a configured address led (that domain names its own DAV server), and to other hosts of a known provider family (`*.icloud.com`, whose home sets live on `pNN-contacts.icloud.com`). A redirect, home set or href anywhere else is refused with `Protocol` and nothing is sent there; a sibling host under the same parent domain is not trusted, since without a public-suffix list a parent domain can be shared by strangers. Enter such an address as the configured one to use it.
- **Sizes.** Listings and multiget answers are capped at 64 MiB, one item at 16 MiB.
- **Busy servers.** `429`, `503` and `504` are waited out as `Retry-After` (seconds) says, at most two minutes, five tries at most, then `Connection`.
- **Redirects.** At most five, followed with the same method and body.
- **Logs** carry counts only.

## Tests

`src/tests.rs` runs an in-process fake CardDAV and CalDAV server on `127.0.0.1` (axum): well-known redirect, principal, home sets, collection listing with ETags, ctags, sync tokens, display names, colors and components, `sync-collection` with `404` removals, `507` paging and an invalid-token error, multigets, `GET`, Basic and Bearer sign-in, and `503` with `Retry-After`. The conformance suite runs through discovery, and again with neither `sync-collection` nor multiget; further tests cover a single-collection address, a refused password, a redirect to another server (nothing reaches it), paging and waiting, a cut list whose sync token does not move on (refused at the second page), refused addresses, and an item the server cannot send (its multiget and its `GET` answer `500`) left out while the rest of the batch is returned. Further tests register WebDAV-Push with a collection that offers it (the stored subscription, the depth the collection lists, the `Expires` header, a notice encrypted for the keys and read back, a second registration replacing the first, `unsubscribe` twice), ask nothing of a collection that does not, and turn the server's refusals into `None` or `Protocol`. The fake also takes `PUT` and `DELETE` with `If-Match` and `If-None-Match`, answers `OPTIONS` with or without `calendar-auto-schedule`, lists `max-resource-size` and `calendar-user-address-set`, and can refuse writes with any status or leave out the `ETag` on `PUT`. `writer_conformance` from `holtmail-collection` runs against it in an auto-scheduling calendar and in an address book; further tests cover the href made from a key and a different item under the same key, a `PUT` without an `ETag`, `max-resource-size`, the wrong content type, `403` and `415` as `Refused` with the precondition, `401` as `Auth`, an unknown collection, and answers with and without a scheduling server (`Schedule-Reply: F`, a stranger's invitation refused, `with_addresses`). `src/ical.rs` has unit tests for quoted parameters, folded lines, series and occurrence answers and refusals. A test against a TLS server in the process with a certificate from `holtmail_core::tls::testing::TestCa` checks that its CA is trusted only through `with_roots`.

Against real servers through the binary ([`tests/integration`](../../../tests/integration/docs/README.md)): Radicale 3.8, Baïkal 0.10 (sabre/dav), Stalwart 0.16 and, with its profile, Nextcloud 35 in `dav.rs`, `dav_faults.rs`, `dav_calendar.rs`, `dav_people.rs`, `dav_export.rs` and `dav_scale.rs` (discovery through the well-known addresses, `sync-collection`, refused tokens, ETag listings without `sync-collection`, `500`s, wrong passwords, broken items, 2 000 cards).
