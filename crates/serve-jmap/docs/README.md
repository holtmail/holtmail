# holtmail-serve-jmap

JMAP (RFC 8620 core, RFC 8621 mail, RFC 8887 WebSocket) for the user's own mail clients. A `JmapServer` is an axum router over the served mail view (`MailView`, `MailActions`), `Submission`, `DeviceAuth` and a `BlobStore` for uploads. A client signs in with a device's app password; every read goes through the mail view as that device sees it, and every write is the same held mail change, local flag, draft or outbox row the web app makes, so it meets the same five locks. A protocol front end, not a contract implementation: it holds no writer, transport or store handle that writes, and only `apps/server` builds it, behind `[serve.jmap]`, merging its routes into the main listener ([apps/server](../../../apps/server/docs/README.md)). Design in [SERVE.md, JMAP](../../../docs/SERVE.md#jmap).

## API

```rust
JmapServer::new(settings: Settings, services: Services) -> JmapServer   // inside a Tokio runtime
jmap_server.router() -> axum::Router
pub struct Settings { public_url, instance_token, trusted_proxies, undo_window, limits: Limits }
pub struct Limits { max_size_upload, max_concurrent_upload, max_size_request, max_concurrent_requests, upload_budget,
                    max_calls_in_request, max_objects_in_get, max_objects_in_set, max_query_limit }
pub struct Services { view, actions, submission, devices, blobs, hub }
pub trait Hub { fn listen(archive, user) -> Box<dyn Wake>; async fn archive_name(archive) -> Option<String>; }
pub trait Wake { async fn changed(&mut self) -> Option<u64>; }
pub const INSTANCE_CAPABILITY: &str = "urn:holtmail:params:jmap:instance";
```

- `Settings::new(public_url)`: default limits (50 MiB uploads, 4 concurrent uploads and requests, 10 MB requests, 32 calls, 500 objects per get and set, 1 000 ids per query page, 200 MiB of live uploads per user), no instance token, no trusted proxies, the instance's undo window.
- `Hub` is what the instance gives besides the contracts: `listen` wakes a push stream when something the user may see changed in the archive (`apps/server` implements it over its `ChangeHub`), and `archive_name` names the JMAP account. A `Wake` returns `None` once no more wake-ups come.
- `JmapServer::new` starts one task following `DeviceAuth::revocations`; it drops cached sign-ins of a revoked device and ends its push streams.

## Routes

| Route | What it does |
| --- | --- |
| `GET /.well-known/jmap` | `307` to `<public_url>/jmap/session`, without sign-in |
| `GET /jmap/session` | The session resource |
| `POST /jmap/api` | Method calls |
| `GET /jmap/download/{account}/{blob}/{name}?accept=` | Blob bytes; `404` for any blob the device may not read |
| `POST /jmap/upload/{account}` | Stores the body in the upload store, `201` with `{accountId, blobId, type, size}`; `403` for a device without `write:mail` or `send:mail` |
| `GET /jmap/eventsource?types=&closeafter=&ping=` | Push as server-sent events |
| `GET /jmap/ws` | RFC 8887 WebSocket, subprotocol `jmap` |

Every `/jmap` route answers CORS preflights (`204`) and allows any origin, never with credentials: a cookie session never signs in here. Credentials a browser kept are never usable from another site: a cross-site navigation or `no-cors` request (`Sec-Fetch-Site` `cross-site` or `same-site` with `Sec-Fetch-Mode` `navigate` or `no-cors`) is refused with `403` before sign-in, `POST /jmap/api` takes only `application/json` (with parameters) and answers `notJSON` otherwise, which makes a browser preflight it, and a WebSocket handshake whose `Origin` is not the `Host` is refused with `403`, since WebSockets bypass CORS.

## What it guarantees

1. **Sign-in.** Every `/jmap` request needs `Authorization: Basic base64(login:app password)` or `Authorization: Bearer <login>:<app password>`, verified as `DeviceProtocol::Jmap`. Without it, or with a wrong, revoked or unusable device, `401` with `WWW-Authenticate: Basic realm="Holtmail"`, or `Bearer realm="Holtmail"` when a browser sent the request (`Sec-Fetch-Mode` or `Origin` present), so a browser never prompts for the app password and never keeps it; too many failures `429` with `Retry-After`; an unavailable check `503`. Sign-in is refused (`403`) unless `public_url` is `https` or the client is on loopback. The client address is the peer, or the rightmost untrusted `X-Forwarded-For` hop when the peer is in `trusted_proxies`. A verified secret is cached for at most 10 seconds, and the revocation feed drops it at once.
2. **Accounts.** One JMAP account per archive the device covers (`Principal::archives`), id = archive id, name from `Hub::archive_name`. `isReadOnly` is true when no mailbox allows any change and the device may send as no identity.
3. **Reads go through the mail view only.** `Mailbox` is the view's tree, `Email` its envelopes, flags, raw bytes and MIME structure, `Thread` its threads; states are `ViewState` per type; `/changes` is `MailView::changes`.
4. **Writes go through `MailActions` and `Submission` only,** with the outcome per object: refused actions are `forbidden` (`notFound` for an email not in the mailbox).
5. **Push.** A stream wakes only through `Hub`, compares each type's state with the last one it sent, and sends a `StateChange` only for types that moved. EventSource: `ping` events every `ping` seconds (at least 10, none for 0), `closeafter=state` closes after the first change, `X-Accel-Buffering: no`. WebSocket: requests, responses, `WebSocketPushEnable` and `Disable`. Revoking the device, or `DeviceAuth::end_sessions`, closes both kinds of stream at once.
6. **Limits** from `Limits`: more than `max_concurrent_requests` requests of one user at once, more calls or a larger body give the RFC 8620 `limit` error; more ids or set objects `requestTooLarge`; uploads over `max_size_upload` (or the mail view's `append_limit`, whichever is smaller) the `limit` error `maxSizeUpload`.

## Methods

| Method | Notes |
| --- | --- |
| `Core/echo` | |
| `Mailbox/get`, `/changes`, `/query` | `myRights` from `MailboxRights`; never `mayCreateChild`, `mayRename`, `mayDelete`; `maySubmit` when the device may send as an identity. Counts from `MailView::status`. Only count changes: `updatedProperties` names the four counts and every selectable mailbox is updated |
| `Mailbox/queryChanges` | `cannotCalculateChanges` |
| `Mailbox/set` | Only `isSubscribed` changes (`MailActions::subscribe`); creates and destroys are `forbidden`, other properties `invalidProperties`: mailboxes come from the accounts |
| `Thread/get`, `/changes` | A merged thread is reported destroyed and its emails updated (the view's guarantee) |
| `Email/get` | List properties from the envelope; `messageId`, `inReplyTo`, `references`, `headers`, `header:*` (all RFC 8621 forms), `bodyStructure`, `textBody`, `htmlBody`, `attachments` and `bodyValues` (with the fetch flags and `maxBodyValueBytes`) from the raw bytes and `MailView::structure`. `keywords` are the effective flags over every place. `Email/get` needs `ids` |
| `Email/changes` | |
| `Email/query` | `inMailbox` (else All Mail), `inMailboxOtherThan`, `before`, `after`, `minSize`, `maxSize`, `hasKeyword`, `notKeyword`, `hasAttachment`, `text`, `from`, `to`, `cc`, `bcc`, `subject`, `body`, `header`, with `AND`, `OR`, `NOT`; sort by `receivedAt` only (default newest first), `collapseThreads`, `position`, `anchor`, `anchorOffset`, `limit`, `calculateTotal`; `canCalculateChanges` false |
| `Email/queryChanges` | `cannotCalculateChanges` (v1, as SERVE.md says) |
| `SearchSnippet/get` | Computed here: words of the `text`, `subject`, `body`, `from`, `to`, `cc`, `bcc` conditions marked with `<mark>` where they start a word in the subject and the first text body, as the index matches |
| `Email/set` | `keywords` and `mailboxIds`, whole or patched; creates; destroys; `ifInState` |
| `Email/import` | From an upload of the user's, or any blob they may read |
| `Identity/get`, `/changes` | `Submission::identities`; `mayDelete` false. The state is a hash of the list; `/changes` from another state is `cannotCalculateChanges` |
| `Identity/set` | Every object `forbidden`: identities are made in the web app |
| `EmailSubmission/get`, `/changes`, `/query`, `/set` | Through `Submission` (below); `/queryChanges` is `cannotCalculateChanges` |

Not offered: `VacationResponse`, `Email/copy`, `Email/parse`, `PushSubscription`. A `using` entry under `urn:ietf:params:jmap:` that the server does not implement is accepted and its methods answer `unknownMethod` (`jmap-client` always sends `vacationresponse`); any other unknown capability is `unknownCapability`.

### Email/set

- **Keywords:** `$seen`, `$flagged`, `$answered`, `$draft` are `\Seen`, `\Flagged`, `\Answered`, `\Draft`; other keywords are flags of the same name; output keywords are lowercase, and `\Deleted` and other system flags never show. The change is the difference to the shown keywords, through `MailActions::set_flags` over every place (held changes or local flags).
- **mailboxIds:** `all` and `kept` are computed and ignored in a change. Removed and added mailboxes: the first pair is `MailActions::move_to`, further added ones `copy_to`, further removed ones `remove` (an email already gone from one of them, as happens when a role mailbox and an `Accounts/` folder show the same place, counts as removed). Leaving no mailbox is `invalidProperties`. A refusal stops that email's update after the actions already done.
- **destroy:** `MailActions::remove` over every place; the email then shows in `Only in Holtmail`, and destroying an email already only there is `forbidden`. A draft is deleted from Drafts.
- **create:** the object is built into RFC 5322 bytes (headers, `headers`, raw `header:*`, `textBody`/`htmlBody`/`attachments` with `bodyValues` or blobs, or `bodyStructure`) and taken in with `MailActions::append` into the first mailbox (Drafts makes a draft), then `copy_to` the others. `id`, `blobId`, `threadId`, `size`, `hasAttachment` and `preview` cannot be given.

### EmailSubmission

- **Create:** the identity must be one of `Submission::identities`; the email's bytes are queued as that identity with `Bcc` and `Resent-Bcc` removed, `Date` and `Message-ID` (`<uuidv7@identity-domain>`) added when missing, and every line ended with CRLF (bare CR and LF become CRLF, a missing last line end is added), as the outbox takes only such messages. Without an `envelope`, the return path is the identity's address and the recipients are `To`, `Cc` and `Bcc` in that order, each once. The idempotency key hashes the device, the email id and the creation id, so a retried request is one message and a reused creation id in a later request is not.
- **undoStatus:** `pending` while queued, `canceled` after an undo, else `final`; setting `canceled` calls `Submission::undo` (`cannotUnsend` once it left).
- **deliveryStatus** per recipient from `Submission::status`: `queued`, `unknown` once a server took it, `no` when refused or bounced, with the server's reply.
- **onSuccessUpdateEmail**, **onSuccessDestroyEmail:** run after the queue accepted, as an implicit `Email/set` answer after the `EmailSubmission/set` one. Clients see the draft destroyed and the sent copy arrive in Sent as a new email (through the outbox), not the draft moving.
- **State:** the submissions made through this server are kept in memory per user and archive; the state carries a per-process epoch, so after a restart `/changes` from an old state is `cannotCalculateChanges` and `/get` without ids lists only newer submissions. Unsettled ones are refreshed from `Submission::status` on every read and push wake-up.

## Ids

JMAP ids stay inside `[A-Za-z0-9_-]`, so the mail view's keys are written differently:

| Object | Id |
| --- | --- |
| Account | The archive id |
| Mailbox | `all`, `kept`, `drafts`, `role-<role>`, `folder-<folder id>`, `node-<base64url path>` |
| Email | `M<message id>`, or `D<draft id>_<version>` for a draft version |
| Thread | `T<thread id>`; a draft's thread is `T` + its email id |
| Blob | `B<BLAKE3 hex>` for an email's raw bytes and for uploads; `P<part with - for .>_<email id>` for one decoded MIME part (`0` is a single-part body) |
| Identity | The identity id |
| EmailSubmission | `S<outbox id>` |

## Deviations, documented

- **Bearer tokens** carry `login:app password`: `DeviceAuth` verifies a secret together with its login. A login-free token waits for device tokens (CLIENTS Phase 44).
- **Thread counts:** `totalThreads` and `unreadThreads` report the email counts.
- **Unified layout:** with several accounts, `inMailboxOtherThan` ignores `Accounts/` folders for an email that is in a role mailbox, because they show the same places; excluding Trash therefore drops what is in Trash.
- **Dates in `before`/`after`** at the top level are exact; inside `OR` and `NOT` they compare whole UTC days, as the mail view does.
- **Search** matches words and word beginnings, never a piece inside a word.
- **Uploads** are usable by their uploader for 24 hours after upload (kept in memory: a restart forgets them). `Services::blobs` keeps uploads and nothing else: an expired upload is deleted from it, unless another live upload holds the same bytes, by an hourly sweep and on the next upload, and the instance gives the server a store of its own that it empties at start. Importing or sending copies the bytes into the archive through the mail view or the submission. One user's live uploads may hold at most `Limits::upload_budget` bytes (200 MiB; not advertised), then `limit` `maxSizeUpload`.
- **Part blobs** name their email instead of the raw hash, so a download cuts the part without a lookup by hash.

## Never logged

App passwords, logins, mailbox names, search text, message content and addresses. Logs carry error kinds only.

## Tests

`cargo test -p holtmail-serve-jmap`: requests through `tower::ServiceExt::oneshot` against the mail view's `FakeMailView` (two accounts), the submission's `FakeSubmission` (mapped onto the view's owner and archive), an in-memory blob store, a test `DeviceAuth` and a test `Hub`: discovery, sign-in, CORS, plain-http refusal, request errors and limits, mailbox rights for writing and read-only devices, subscriptions, query and get with result references, paging and threads, bodies and headers, attachments and part downloads, raw bytes matching their hash, search and snippets, keywords and `/changes`, `ifInState`, moves and destroys, local flags, uploads and import, the upload scope check and budget, expired uploads deleted unless shared, the browser guards (no `Basic` challenge for a browser, cross-site navigation refused, `notJSON` for other media types, foreign-origin WebSocket refused), drafts sent with `onSuccessDestroyEmail` and undone, a delivered message that cannot be undone, EventSource pushes, `closeafter=state`, revocation ending a stream. Loopback over TCP on 127.0.0.1: `holtmail-source-jmap` reading the served inbox byte for byte, its `JmapWriter` flagging served mail, `holtmail-submit-jmap` checking and sending through the served account (`Email/import` into Drafts and `EmailSubmission/set`), the loop guard refusing this instance, `jmap-client` 0.4 querying, downloading and flagging, and a WebSocket request, push and revocation. On the real binary: the `serve_jmap` integration suite ([tests/integration](../../../tests/integration/docs/README.md)).
