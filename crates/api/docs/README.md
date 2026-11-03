# holtmail-api

The HTTP API the web app and scripts use. Built on `axum`; the OpenAPI description is generated with `utoipa` from the handlers and served at `/api/openapi.json` (`holtmail openapi` prints it). Response types are the API's own, separate from the store's, so the store can change without changing the API.

## Usage

```rust
let app = holtmail_api::router(ApiState { blobs, store, index, auth, mailboxes, secure_cookies });
axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
```

`ConnectInfo<SocketAddr>` gives the login rate limit the client address; without it every client counts as `0.0.0.0`.

## Mailboxes

Talking to mail servers, sealing passwords and reading mailbox files need implementation crates, which this crate may not depend on. The server hands them in as `mailboxes: Arc<dyn Mailboxes>`:

| Method | Guarantees |
| --- | --- |
| `add_imap(archive, NewImapAccount)` | Signs in and lists folders first; nothing is stored when that fails (`MailboxError::Invalid` with a message safe to show). Then stores the account with its password sealed and starts its first sync in the background |
| `set_password(account, password)` | Signs in with the account's settings and the new password, then stores it sealed; nothing changes when signing in fails |
| `sync(account)` | Starts a sync in the background; does nothing when one of this account is running |
| `import(account, file)` | Imports the uploaded file into the import account in the background and removes the file afterwards |
| `upload_dir()` | Where uploads are written before `import` |
| `status(account)` | `AccountStatus { running, new_messages, finished_at, error }` of the running or last run |

## Sign-in

| Method and path | Body | Answers |
| --- | --- | --- |
| `GET /api/setup` | | `200 { "needed": bool }`: `true` while there are no users |
| `POST /api/setup` | `{ "token", "email", "name", "password" }` | `200 { "user" }` and the session cookie; `400` for bad input (password under 10 characters, malformed email, empty name); `403` for a wrong or used token or when users exist |
| `POST /api/login` | `{ "email", "password" }` | `200 { "user" }` and the session cookie; `401` for a wrong email or password (the same message for both); `429` with `Retry-After` seconds after 10 failures from one address for one email within 15 minutes |
| `POST /api/logout` | none or `{}` | `204` and a cookie that expires the session; idempotent |
| `GET /api/session` | | `200 { "user" }` or `401` |

`user` is `{ "id", "email", "name", "role" }` with `role` one of `owner`, `admin`, `member`. Sign-in answers carry `Cache-Control: no-store`.

The setup token comes from the setup link the server prints at start while there are no users (`/setup?token=...`); see [holtmail-auth](../../auth/docs/README.md).

### Session cookie

`holtmail_session=<token>; HttpOnly; SameSite=Lax; Path=/; Max-Age=2592000`, plus `Secure` when `secure_cookies` is set (the server sets it for an `https` `public_url`) or the request has `X-Forwarded-Proto: https`. The token is 43 base64url characters; only its hash is stored. Sessions last 30 days from sign-in.

### Access and CSRF

- Every route except the five above and `/api/openapi.json` answers `401 { "error": "sign in required" }` without a live session. Unknown paths under `/api/` answer `401` too, and `404` once signed in.
- Owners and admins may do everything. Members may read the archives granted to them (`GET` on `/api/archives…`, `/api/messages…`, `/api/threads…`) and change their own profile (`/api/me…`); archives, messages and threads they may not read answer `404` as if missing, and every other write, `/api/users…` and `/api/accounts…` answer `403`. `GET /api/archives` lists only readable archives.
- Requests other than `GET`, `HEAD` and `OPTIONS` must have `Content-Type: application/json` or, for uploads, `application/octet-stream` (else `415`); neither can be sent cross-site without a CORS preflight, which the server never grants and are refused with `403` when they carry an `Origin` whose host differs from `Host`, or `Sec-Fetch-Site: cross-site`. Together with `SameSite=Lax` this keeps other sites from acting with a visitor's session.
- The OpenAPI description declares the cookie as the `session` security scheme; the sign-in routes are marked as needing none.

## Endpoints

| Method and path | Returns |
| --- | --- |
| `GET /api/archives` | Archives with totals |
| `GET /api/archives/{id}` | One archive |
| `GET /api/archives/{id}/accounts` | Its accounts |
| `GET /api/archives/{id}/folders` | Its folders with message and `unread` counts (`unread`: not removed, without `\Seen`) |
| `GET /api/archives/{id}/messages?folder&after&limit` | A page of message summaries, newest first; `next` continues. Each summary has `flags` (in `folder` when given, else the union over the folders it is in), `thread_count` and `folder_ids` |
| `GET /api/archives/{id}/search?q&sort&offset&limit` | Search with the [query language](../../query/docs/README.md); `400` with the parser's message for a bad query |
| `GET /api/messages/{id}` | The reader's view: headers, `preview`, `in_reply_to`, `references`, plain text, attachments, folders |
| `GET /api/messages/{id}/body?remote=block\|proxy\|direct` | The sanitized HTML body as a full document (`text/html`); a plain text body is escaped into `<pre>`. See [Remote images](#remote-images) |
| `GET /api/messages/{id}/remote/{key}` | One remote image of the message: the archive's saved copy, or fetched now by the server. `404` when the message links no such image or the server will not fetch it, `502` when the remote server fails |
| `GET /api/messages/{id}/raw` | The original bytes as `message/rfc822`, named after the subject |
| `GET /api/messages/{id}/attachments/{ordinal}` | Attachment bytes |
| `GET /api/threads/{id}` | Every message of the conversation, oldest first |

Accounts carry `status`: `{ running, new_messages, finished_at, error }` of the running or last sync or import, kept in memory by the server.

### Managing

Owners and admins only.

| Method and path | Body | Answers |
| --- | --- | --- |
| `POST /api/archives` | `{ "name" }` | `201` archive; `400` for an empty name or one over 100 characters |
| `PATCH /api/archives/{id}` | `{ "name"?, "save_remote_images"? }` | `200` archive; turning saving on starts saving the images of messages already archived |
| `DELETE /api/archives/{id}` | | `204` after removing the archive, its index entries and the blobs no other archive uses; `409` while one of its accounts syncs or imports |
| `POST /api/archives/{id}/accounts` | `{ "host", "port"?, "security"? (tls, starttls, plain), "username", "password", "name"? }` | `201` account, first sync started; `400` with a readable reason when signing in fails |
| `POST /api/archives/{id}/imports?name=` | the file, `application/octet-stream`, any size | `202` with the import account named after the file (reused for the same name); the import runs in the background. `400` for an empty file |
| `PATCH /api/accounts/{id}` | `{ "name"?, "enabled"?, "password"? }` | `200` account; a new password is checked by signing in first |
| `DELETE /api/accounts/{id}` | | `204`; its messages stay in the archive. `409` while it syncs or imports |
| `POST /api/accounts/{id}/sync` | none or `{}` | `202` account; `400` for an import account |
| `GET /api/users` | | Every user with `created_at` and the `archives` granted to them |
| `POST /api/users` | `{ "email", "name", "password", "role"?, "archives"? }` | `201`; `409` for a taken email; only an owner adds owners (`403`) |
| `PATCH /api/users/{id}` | `{ "email"?, "name"?, "role"?, "archives"?, "password"? }` | `200`; a password ends their sessions. Only an owner changes owners or makes one; nobody changes their own role (`400`) |
| `DELETE /api/users/{id}` | | `204`; not yourself (`400`), owners only by owners |

### Your profile

| Method and path | Body | Answers |
| --- | --- | --- |
| `PATCH /api/me` | `{ "name"?, "email"? }` | `200 { "user" }` |
| `POST /api/me/password` | `{ "current", "new" }` | `200 { "user" }` with a new session cookie; every other session ends. `401` for a wrong current password |

The OpenAPI description declares every path parameter, the `x-holtmail-remote-blocked` header of `/body`, `Retry-After` on a `429` from `/api/login`, and the `400`, `401`, `404` and `500` answers each route can give.

Errors are `{ "error": "..." }` with `400`, `401`, `403`, `404`, `409`, `415`, `429` or `500`. Ids that do not parse answer `404`, like ids that do not exist.

## Safety

- `/body` runs [`holtmail-sanitize`](../../sanitize/docs/README.md). Inline `cid:` images point at absolute attachment URLs built from the request's `Host` and `X-Forwarded-Proto`.
- The web app shows `/body` in an iframe with `sandbox="allow-same-origin allow-popups allow-popups-to-escape-sandbox"` and no `allow-scripts`; the document's own CSP forbids scripts as well.
- Attachments are served inline only for images and PDF; everything else, including HTML and SVG, is `application/octet-stream` with `Content-Disposition: attachment`. Every download has `X-Content-Type-Options: nosniff` and `Content-Security-Policy: sandbox`.
- File names in `Content-Disposition` are cleaned and sent both as ASCII and as RFC 5987 UTF-8.

## Not yet

Passkeys, OIDC, API tokens, retention and export come with the milestones in [PLAN.md](../../../docs/PLAN.md).

## Remote images

`?remote=` on `/body` decides what happens to images and backgrounds hosted elsewhere:

- `block` (default): left out; `x-holtmail-remote-blocked: 1` tells the reader to offer them.
- `proxy`: pointed at `/api/messages/{id}/remote/{key}`, so the server fetches them and the reader's address stays private. Remote styles and fonts are still left out.
- `direct`: original addresses kept and allowed by the document's CSP; the reader's browser contacts the sender's servers.

Images the archive saved (see `save_remote_images`) are always pointed at `/remote/{key}`, whatever the mode, because showing them reaches no one.

`/remote/{key}` serves only URLs that the message itself links, so it is not an open proxy. It answers with the saved copy when there is one (cached for a year) and otherwise fetches through the [`Fetcher`](../../fetch/docs/README.md), which never contacts private or local addresses (cached for a day). Responses carry `nosniff` and `Content-Security-Policy: default-src 'none'; sandbox`; types other than common raster images are sent as `application/octet-stream`.

`ApiState.fetcher` is the `Fetcher`; `Mailboxes::save_remote_images(archive)` starts saving in the background.

