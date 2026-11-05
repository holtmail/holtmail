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
| `configure(account, AccountChange)` | Changes the connection (`connection` for IMAP, `jmap` for JMAP), password or token and sync interval; a new connection or password is checked by signing in first and nothing changes when that fails |
| `sync(account)` | Starts a sync in the background; does nothing when one of this account is running |
| `import(account, file)` | Imports the uploaded file into the import account in the background and removes the file afterwards |
| `upload_dir()` | Where uploads are written before `import` |
| `status(account)` | `AccountStatus { running, new_messages, finished_at, error }` of the running or last run |
| `add_jmap(archive, NewJmapAccount)` | Signs in to the JMAP server and lists its mailboxes, stores the account with its password or token sealed and starts its first sync; nothing is stored when signing in fails |
| `details(account)` | `AccountDetails { connection, jmap, sync_interval_minutes, min_age_days, oauth_provider, needs_sign_in }` as stored |
| `start_oauth(OAuthStart)` | A Gmail or Microsoft sign-in for `OAuthTarget::NewAccount { archive, name }` or `OAuthTarget::Account(id)`, for a new account of `kind` `Imap` or `Graph` (Microsoft only); signing in again keeps the account's own kind. Returns the provider's page, a `state` valid for a few minutes and once, whether the answer must be pasted (`needs_paste`) and whether Thunderbird's client is used. `Invalid` for an unknown provider, archive or account, Graph with Google, or an account that is neither IMAP nor Graph |
| `finish_oauth(answer)` | From the pasted address or the callback's query: exchanges the code, checks that the mail server (IMAP, or Graph by listing the folders) accepts the token, stores the refresh token sealed, then starts a sync. Creates the account, or replaces an existing account's sign-in (moving a password account to OAuth). Nothing is stored when a step fails; `Invalid` with a reason safe to show |
| `oauth_return_to(answer)` | The page a waiting sign-in returns to, without using it up |

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

- Every route except the five above, `/api/oauth/callback` and `/api/openapi.json` answers `401 { "error": "sign in required" }` without a live session. Unknown paths under `/api/` answer `401` too, and `404` once signed in.
- Owners and admins may do everything. Members may read the archives granted to them (`GET` on `/api/archives…`, `/api/messages…`, `/api/threads…`) and change their own profile (`/api/me…`); archives, messages and threads they may not read answer `404` as if missing, and every other write, `/api/users…`, `/api/accounts…` and `/api/oauth…` answer `403`. `GET /api/archives` lists only readable archives.
- Requests other than `GET`, `HEAD` and `OPTIONS` must have `Content-Type: application/json` or, for uploads, `application/octet-stream` (else `415`); neither can be sent cross-site without a CORS preflight, which the server never grants and are refused with `403` when they carry an `Origin` whose host differs from `Host`, or `Sec-Fetch-Site: cross-site`. Together with `SameSite=Lax` this keeps other sites from acting with a visitor's session.
- The OpenAPI description declares the cookie as the `session` security scheme; the sign-in routes are marked as needing none.

## Endpoints

| Method and path | Returns |
| --- | --- |
| `GET /api/archives` | Archives with totals |
| `GET /api/archives/{id}` | One archive |
| `GET /api/archives/{id}/accounts` | Its accounts with `status` (including `next_sync_at`), `connection` (host, port, security, user name and excluded folders of IMAP accounts; `null` for members and other kinds; never the password), `jmap` (`url`, `username`, `auth` and `exclude` of JMAP accounts; `null` for members and other kinds; never the password or token), their own `sync_interval_minutes`, `min_age_days` (only mail at least that many days old is archived; `null` for all mail), `oauth_provider` (`google`, `microsoft` or `null` for a password) and `needs_sign_in` (the provider no longer accepts the sign-in; it does not sync until someone signs in again) |
| `GET /api/archives/{id}/folders` | Its folders with message and `unread` counts (`unread`: not removed, without `\Seen`) |
| `GET /api/archives/{id}/labels` | `LabelDto[]`: every label carried by at least one message (Gmail), by name, with `messages` and `unread` (not read in any folder the message is in); empty for archives without labels |
| `GET /api/archives/{id}/messages?folder&label&after&limit` | A page of message summaries, newest first; `next` continues. `label` keeps messages carrying that exact label, alone or with `folder`. Each summary has `flags` (in `folder` when given, else the union over the folders it is in), `thread_count`, `folder_ids` and `labels` (sorted) |
| `GET /api/archives/{id}/search?q&sort&offset&limit` | Search with the [query language](../../query/docs/README.md); `400` with the parser's message for a bad query. Each summary has `attachment_match`: `true` when the words were found only in text inside its attachments (from the index's `attachment_hits`; always `false` in plain lists) |
| `GET /api/archives/{id}/export?format=mbox\|zip&folder&label&q` | Downloads the messages with [`holtmail-export`](../../export/docs/README.md): the whole archive, or one folder (id), label or search, as one mbox file (`application/mbox`) or a zip of `.eml` files in folder directories (`application/zip`). `Content-Disposition: attachment` with a name like `Personal - INBOX.mbox`, `Cache-Control: no-store`, `nosniff`. The body is streamed while it is written (chunked, about one message in memory); an export that fails part way ends the response without its final chunk, so clients see a broken download rather than a short file that looks complete. `400` for another format, more than one of `folder`, `label` and `q`, or a query that does not parse; `404` for an unknown archive or a folder of another archive. A `GET`, so members can export the archives granted to them and anything in them, like they can read it; the session cookie goes along with a plain link (`SameSite=Lax`). Messages whose bytes are missing are left out; the log line at the end carries the counts |
| `GET /api/messages/{id}` | The reader's view: headers, `preview`, `in_reply_to`, `references`, plain text, attachments (each with `url` and, for PNG, JPEG, GIF and WebP up to 25 MiB, `thumbnail_url`), folders, `labels` |
| `GET /api/messages/{id}/body?remote=block\|proxy\|direct` | The sanitized HTML body as a full document (`text/html`); a plain text body is escaped into `<pre>`. See [Remote images](#remote-images) |
| `GET /api/messages/{id}/remote/{key}` | One remote image of the message: the archive's saved copy, or fetched now by the server. `404` when the message links no such image or the server will not fetch it, `502` when the remote server fails |
| `GET /api/messages/{id}/raw` | The original bytes as `message/rfc822`, named after the subject |
| `GET /api/messages/{id}/attachments/{ordinal}` | Attachment bytes |
| `GET /api/messages/{id}/attachments/{ordinal}/thumbnail` | A preview at most 320 px on its longest side, JPEG or PNG (with transparency), made by [`holtmail-thumbnail`](../../thumbnail/docs/README.md) on first request and kept as a derived blob (`DerivedKind::Thumbnail`), so later requests read it back. `404` for other types and for images that do not decode; that answer is recorded and the image is not decoded again. At most two thumbnails are made at a time |
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
| `POST /api/archives/{id}/accounts/jmap` | `{ "url", "auth"? (password, token), "username"?, "password", "name"? }`: `url` is the session address (Fastmail: `https://api.fastmail.com/jmap/session`) or the server's address, where `/.well-known/jmap` is asked; `password` holds the password or the API token; `username` is needed for `password` | `201` account (`kind: "jmap"`), first sync started; `400` with a readable reason for an empty field, an unknown `auth`, an address without https, or when signing in fails |
| `POST /api/archives/{id}/imports?name=` | the file, `application/octet-stream`, any size | `202` with the import account named after the file (reused for the same name); the import runs in the background. `400` for an empty file |
| `PATCH /api/accounts/{id}` | `{ "name"?, "enabled"?, "password"?, "connection"?: { "host", "port"?, "security"?, "username", "exclude"? }, "jmap"?: { "url", "auth"?, "username"?, "exclude"? }, "sync_interval_minutes"?, "min_age_days"? }` | `200` account. A new connection or password is checked by signing in first (with the stored password when none is given) and nothing is stored when that fails; another server (IMAP host or port, a move to `plain`, or a JMAP address on another origin) without `password` is `400` "Enter the password again when changing the server." (or "the token" for a JMAP token account), so a stored secret never reaches a server it was not given for; an OAuth account's host, port and security cannot change (`400`); `connection` is for IMAP accounts and `jmap` for JMAP accounts (the other is `400`); `password` is a JMAP token account's new token, and moving a JMAP account between `password` and `token` needs it; `400` while the account syncs. `sync_interval_minutes`: minutes, `0` for only on request, `null` to follow the server's again; a missing key keeps it. `min_age_days`: 1 to 3650, `0` or `null` archives all mail again (`400` above), a missing key keeps it |
| `GET` / `PUT /api/sync-schedule` | `{ "minutes" }` | The interval of accounts without their own, with `from_config`; `PUT` answers `400` above a week or when the server's config sets it |
| `DELETE /api/accounts/{id}` | | `204`; its messages stay in the archive. `409` while it syncs or imports |
| `POST /api/accounts/{id}/sync` | none or `{}` | `202` account; `400` for an import account or one that needs sign-in |
| `POST /api/oauth/start` | `{ "provider" (google, microsoft), "email", "archive_id"? , "account_id"?, "name"?, "kind"? (imap, graph), "return_to"? }` | `200 { authorize_url, state, needs_paste, thunderbird_client, expires_at }`. Exactly one of `archive_id` (a new account) and `account_id` (sign in again to an IMAP or Graph account, or move a password IMAP account to OAuth); `kind` is how a new account reads mail, `imap` by default or `graph` for Microsoft Graph (only with `microsoft`, else `400`), and is ignored for `account_id`, whose own kind decides; `email` may be empty when the account already uses OAuth. `return_to` is a path in the web app (`/settings` when missing); anything else is `400`. `404` for an unknown archive or account |
| `POST /api/oauth/finish` | `{ "pasted_url" }` | `200` account (`kind: "graph"` for a Graph sign-in, with the address Graph names), sync started. `400` with a readable reason for text that is not the provider's answer, an unknown, used or expired sign-in, access refused, a token the mail server or Graph refuses, or a sign-in again as another mailbox than a Graph account archives |
| `GET /api/oauth/callback?code&state` | | No session needed: the `state` is the credential. Finishes like `/api/oauth/finish` and answers `303` to the sign-in's `return_to` with `oauth=done&account=<id>`, or with `oauth_error=<reason>` (to `/settings` when the sign-in is unknown). Only used by own clients whose redirect points here |
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
- Attachments are served inline only for images and PDF; everything else, including HTML and SVG, is `application/octet-stream` with `Content-Disposition: attachment`. Every download, thumbnails included, has `X-Content-Type-Options: nosniff` and `Content-Security-Policy: sandbox`, and goes through the same access check as its message. Thumbnails are re-encoded by the server, never the sender's bytes.
- File names in `Content-Disposition` are cleaned and sent both as ASCII and as RFC 5987 UTF-8.

## Not yet

Passkeys, OIDC, API tokens, retention and export come with the milestones in [PLAN.md](../../../docs/PLAN.md).

## Remote images

`?remote=` on `/body` decides what happens to images and backgrounds hosted elsewhere:

- `block` (default): left out; `x-holtmail-remote-blocked: 1` tells the reader to offer them.
- `proxy`: pointed at `/api/messages/{id}/remote/{key}`, so the server fetches them and the reader's address stays private. Remote styles and fonts are still left out.
- `direct`: original addresses kept and allowed by the document's CSP; the reader's browser contacts the sender's servers.

Images the archive saved (see `save_remote_images`) are pointed at `/remote/{key}` whatever the mode, because showing them reaches no one, and `x-holtmail-remote-saved: 1` says so. `?saved=false` ignores saved copies and shows the originals as `remote` says, through `/api/messages/{id}/live/{key}` for `proxy`, which always fetches now. The stored message is never changed: copies live beside it.

`/remote/{key}` serves only URLs that the message itself links, so it is not an open proxy. It answers with the saved copy when there is one (cached for a year) and otherwise fetches through the [`Fetcher`](../../fetch/docs/README.md), which never contacts private or local addresses (cached for a day). Responses carry `nosniff` and `Content-Security-Policy: default-src 'none'; sandbox`; types other than common raster images are sent as `application/octet-stream`.

`ApiState.fetcher` is the `Fetcher`.

### Saving and tasks

`ArchiveDto.remote_images` has `since` (when saving was turned on), `images`, `bytes`, `failed` and `older_pending`: messages archived before `since` and not looked at. Turning saving on starts nothing: new mail is handled after each sync or import, older mail only through `POST /api/archives/{id}/remote-images` with `{ "job": "older" }`, and `{ "job": "failed" }` fetches failed images again. Both answer `202` with the task, or the archive's running remote image task when there is one.

`GET /api/tasks` lists running tasks and recent finished ones (syncs, imports, remote images) with `done`, `total` (`null` when unknown), a `summary` and a `log` of `{ at, level, text }` lines that carry counts, folder names and reasons, never message content. `POST /api/tasks/{id}/cancel` stops a cancellable task after its current step (`409` otherwise). Finished tasks stay listed after a restart (the server keeps the last 200); a task's `state` is `running`, `done`, `failed`, `cancelled`, or `interrupted` when the server stopped while it ran. Task ids are unique across restarts. `TaskKind`, `TaskState`, `TaskLevel` and `TaskLine` are the store's types, re-exported.

`GET` / `PUT /api/image-proxy` read and set the outbound proxy for remote images: `{ "proxy": "socks5h://127.0.0.1:9050" }` or `null`. The answer shows `address` without credentials and `from_config` when the server's config sets it (then `PUT` answers `400`). Setting checks that the proxy accepts connections.

Members get `403` on `/api/tasks`, `/api/image-proxy` and `/api/sync-schedule`. The server provides all of this through `Mailboxes`: `tasks`, `cancel_task`, `start_remote_images`, `image_proxy`, `set_image_proxy`, and for accounts `configure` (connection, password, interval, minimum age; a password for an OAuth account is `400`, a new connection of one is checked with a fresh access token), `details`, `sync_schedule` and `set_sync_schedule`.

