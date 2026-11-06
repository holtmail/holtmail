# holtmail-collection-dav

Reads address books and calendars from CardDAV (RFC 6352) and CalDAV (RFC 4791) servers behind the [`CollectionSource`](../../collection/docs/README.md) contract. Read-only: it sends only `PROPFIND`, `REPORT` and `GET`.

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
3. `PROPFIND` depth 1 on each home set lists collections with `displayname`, `getctag` (any namespace), `sync-token`, `calendar-color` (any namespace) and `supported-calendar-component-set`. Only `addressbook` and `calendar` resource types count.

Collection and item ids are the percent-decoded paths, so servers that encode the same path differently from one answer to the next (`@` and `%40`) give one id; requests encode them again. The collections found are remembered for the calls that follow; an unknown id lists them again, then answers `Gone`.

## Changes and bodies

- `changes` sends `sync-collection` (RFC 6578, `Depth: 0`, `sync-level 1`, `getetag`). A `404` in a response is a removal. A `507` for the collection itself means the server cut the list: the next page is asked with the new token, up to 100 pages and a million changes; a cut list whose new token equals the one asked with is `Protocol`, since it would never end. A refusal of a given token (`valid-sync-token`, or `400`/`403`/`409`/`412`) is `TokenExpired`; a refusal without a token (`403`, `405`, `501`, …) is `Unsupported`.
- `list` is `PROPFIND` depth 1 with `getetag`, leaving out the collection and sub-collections.
- `fetch` sends `addressbook-multiget` or `calendar-multiget` with up to 50 hrefs (`BATCH`). A server that refuses multiget (`400`, `403`, `405`, `415`, `422`, `501`) is read with one `GET` per item from then on. `404` answers are left out. A multiget answered with a server error (`5xx`) is asked again one `GET` per item for that batch only; an item whose `GET` fails too (a `5xx` or no answer) is left out and logged as a count, and the others are returned. sabre/dav (Baïkal, Nextcloud) parses every card of a multiget and answers the whole request with `500` when one of them does not parse, so one broken card would otherwise stop the whole address book; [`holtmail-pim-sync`](../../pim-sync/docs/README.md) counts the left-out item as failed and asks again on the next run.
- `address-data` and `calendar-data` are kept exactly: the XML text is decoded without line-end normalization, so CRLF (raw or as `&#13;`) survives byte for byte.

## Limits and safety

- **Credentials stay home.** They are sent only over https (http only to loopback), to the configured servers, to where a well-known lookup of a configured address led (that domain names its own DAV server), and to other hosts of a known provider family (`*.icloud.com`, whose home sets live on `pNN-contacts.icloud.com`). A redirect, home set or href anywhere else is refused with `Protocol` and nothing is sent there; a sibling host under the same parent domain is not trusted, since without a public-suffix list a parent domain can be shared by strangers. Enter such an address as the configured one to use it.
- **Sizes.** Listings and multiget answers are capped at 64 MiB, one item at 16 MiB.
- **Busy servers.** `429`, `503` and `504` are waited out as `Retry-After` (seconds) says, at most two minutes, five tries at most, then `Connection`.
- **Redirects.** At most five, followed with the same method and body.
- **Logs** carry counts only.

## Tests

`src/tests.rs` runs an in-process fake CardDAV and CalDAV server on `127.0.0.1` (axum): well-known redirect, principal, home sets, collection listing with ETags, ctags, sync tokens, display names, colors and components, `sync-collection` with `404` removals, `507` paging and an invalid-token error, multigets, `GET`, Basic and Bearer sign-in, and `503` with `Retry-After`. The conformance suite runs through discovery, and again with neither `sync-collection` nor multiget; further tests cover a single-collection address, a refused password, a redirect to another server (nothing reaches it), paging and waiting, a cut list whose sync token does not move on (refused at the second page), refused addresses, and an item the server cannot send (its multiget and its `GET` answer `500`) left out while the rest of the batch is returned. A test against a TLS server in the process with a certificate from `holtmail_core::tls::testing::TestCa` checks that its CA is trusted only through `with_roots`.

Against real servers through the binary ([`tests/integration`](../../../tests/integration/docs/README.md)): Radicale 3.8, Baïkal 0.10 (sabre/dav), Stalwart 0.16 and, with its profile, Nextcloud 35 in `dav.rs`, `dav_faults.rs`, `dav_calendar.rs`, `dav_people.rs`, `dav_export.rs` and `dav_scale.rs` (discovery through the well-known addresses, `sync-collection`, refused tokens, ETag listings without `sync-collection`, `500`s, wrong passwords, broken items, 2 000 cards).
