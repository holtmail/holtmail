# holtmail-source-jmap

Reads mail from JMAP servers (RFC 8620 for the core, RFC 8621 for mail), such as Fastmail, Stalwart or Cyrus. It implements the [`holtmail-source`](../../source/docs/README.md) contract: mailboxes become folders, each email's original RFC 5322 bytes are downloaded through the session's `downloadUrl`, and keywords become flags. It never changes anything on the server: it only uses `Mailbox/get`, `Email/query`, `Email/get` and blob downloads.

## Public API

| Item | Guarantees |
| --- | --- |
| `JmapSettings { url, username, auth }` | Stored in the account's settings JSON; the password or token is sealed separately. `from_json` fails with `Config` |
| `JmapAuth::{Password, Token}` | `password` (default): HTTP Basic with `username`. `token`: a bearer token, such as a Fastmail API token; the user name is not sent |
| `session_url(input)` | The session address for what someone typed: `https://` is added when the scheme is missing; an address without a path gets `/.well-known/jmap`. Only `https`, or `http` to a loopback address; no credentials in the address. `Config` otherwise |
| `JmapSource::with_roots(roots)` | Also trusts the CA certificates in `TlsRoots` besides the system's roots; verification stays on. `Config` for an unusable certificate |
| `JmapSource::new(settings, credentials)` | `Credentials::Password` signs in with Basic as `username` (`Config` without one), `Credentials::AccessToken` with a bearer token. `Config` for an unusable address |

## Config

```json
{ "url": "https://api.fastmail.com/jmap/session", "username": "", "auth": "token" }
```

`folders.exclude`, `sync_interval_minutes` and `min_age_days` in the same JSON work as for IMAP accounts.

## How the contract maps onto JMAP

- **Session.** A session address under `/.well-known/` is first followed through its redirects (at most five, never from https to http) **without credentials**; the session is then asked with credentials at the address it led to, which may be on another host (Fastmail sends `fastmail.com` to `api.fastmail.com`). The session request itself does not follow a redirect: credentials never go to a host the typed or well-known address did not lead to, and a session address that redirects is refused as a protocol error saying so. `connect` checks that the session offers `urn:ietf:params:jmap:mail` and uses that capability's primary account, `apiUrl` and `downloadUrl`, also when they are on another host than the session (Fastmail downloads from `www.fastmailusercontent.com`); relative addresses are resolved against the session's address. API calls and downloads follow up to five redirects, never away from https, and reqwest drops the credentials on a redirect to another origin.
- **Folders.** Every mailbox is a folder; paths join names from the top with `/`. JMAP roles (`inbox`, `sent`, `drafts`, `trash`, `junk`, `archive`, `all`, `flagged`, `important`) map to the same `FolderRole`; other mailboxes have none. Every mailbox is selectable and none is a label.
- **Several mailboxes per email.** An email in two mailboxes is a message in two folders, like an IMAP copy: one blob, two locations. Its bytes are downloaded once per folder it is first seen in; ingest stores them once.
- **UIDs.** JMAP has none, so each email gets `receivedAt` in whole seconds shifted left by 22 bits, with the low 22 bits from a 64-bit FNV-1a hash of its id. That UID is the same in every mailbox and session (`receivedAt` never changes), below `2^62`, and ascends with the received time, so minimum-age syncs stop in the right place. It does not ascend with the time a message entered a mailbox (a message moved in later can be older), so `select` reports `uids_ascend: false` and the sync looks for every UID it has not archived yet. Two emails received in the same second with the same 22 hash bits would share a UID; the one with the larger id is skipped and the clash counted in a warning (ids only, never content).
- **`UIDVALIDITY`.** 32 bits of the hash of the mailbox id: a mailbox deleted and created again with the same name renumbers, as on IMAP.
- **Listing.** `uids` pages `Email/query` (`inMailbox`, sorted by `receivedAt` ascending, `calculateTotal`) and reads `receivedAt` with `Email/get` through a back-reference in the same request when `maxCallsInRequest` allows two calls. `fetch` and `flags` use `Email/get` with at most `maxObjectsInGet` ids per call (capped at 500) and at most `maxCallsInRequest` calls per request (capped at 16). Blobs are downloaded four at a time.
- **Flags.** Keywords set to `true` become flags: `$seen`, `$flagged`, `$answered` and `$draft` take IMAP's names (`\Seen`, ...); others stay as JMAP writes them (`$forwarded`). Folders have no mod-sequences, so the sync re-reads flags once a day; `flags` with `changed_since` is a `Protocol` error.
- **Vanished mail.** An email deleted between listing and fetching (not found by `Email/get`, or a `404` download) is left out, as the contract asks.

## Errors

`401` and `403` are `Auth`; `404` on the session address is `Config`; a missing mail capability or mail account is `Config`; `429`, `5xx`, timeouts and broken connections are `Connection`; other refusals, method-level `error` answers and malformed answers are `Protocol`. Error texts name the host, never the address with ids, the user name or the secret.

## Limits and not done

- JSON answers larger than 64 MiB and message downloads larger than 256 MiB are refused as a protocol error.
- `Email/changes` and `Mailbox/changes` are not used: every sync lists each mailbox's ids, as IMAP's `UID SEARCH ALL` does, and reads flags of archived mail once a day.
- A mailbox name containing `/` reads as a deeper path.
- `Mailbox/get` asks for every mailbox at once; a server that answers `requestTooLarge` for that is not handled.
- Tested against the in-process fake JMAP server in `src/tests.rs` (session with a redirect from `/.well-known/jmap`, a well-known redirect to another host asked without credentials, a session address that redirects refused with nothing sent to its target, Basic and bearer sign-in, `Mailbox/get`, paged `Email/query`, `Email/get` with back-references, downloads), a TLS server in the process whose test CA is trusted only through `with_roots`, and Stalwart 0.16 through the real binary in [`tests/integration/tests/jmap.rs`](../../../tests/integration/tests/jmap.rs) (password and API-key sign-in, first and incremental syncs, an email in two mailboxes, moves, deletions, keywords, refused credentials, well-known discovery and redirects). Not yet against Fastmail or Cyrus.
