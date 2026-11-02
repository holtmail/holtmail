# holtmail-api

The HTTP API the web app and scripts use. Built on `axum`; the OpenAPI description is generated with `utoipa` from the handlers and served at `/api/openapi.json` (`holtmail openapi` prints it). Response types are the API's own, separate from the store's, so the store can change without changing the API.

## Usage

```rust
let app = holtmail_api::router(ApiState { blobs, store, index, auth, secure_cookies });
axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
```

`ConnectInfo<SocketAddr>` gives the login rate limit the client address; without it every client counts as `0.0.0.0`.

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
- Requests other than `GET`, `HEAD` and `OPTIONS` must have `Content-Type: application/json` (else `415`) and are refused with `403` when they carry an `Origin` whose host differs from `Host`, or `Sec-Fetch-Site: cross-site`. Together with `SameSite=Lax` this keeps other sites from acting with a visitor's session.
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
| `GET /api/messages/{id}/body?remote` | The sanitized HTML body as a full document (`text/html`); a plain text body is escaped into `<pre>` |
| `GET /api/messages/{id}/raw` | The original bytes as `message/rfc822`, named after the subject |
| `GET /api/messages/{id}/attachments/{ordinal}` | Attachment bytes |
| `GET /api/threads/{id}` | Every message of the conversation, oldest first |

The OpenAPI description declares every path parameter, the `x-holtmail-remote-blocked` header of `/body`, `Retry-After` on a `429` from `/api/login`, and the `400`, `401`, `404` and `500` answers each route can give.

Errors are `{ "error": "..." }` with `400`, `401`, `403`, `404`, `409`, `415`, `429` or `500`. Ids that do not parse answer `404`, like ids that do not exist.

## Safety

- `/body` runs [`holtmail-sanitize`](../../sanitize/docs/README.md): remote content is blocked unless `?remote=true`, and the response header `x-holtmail-remote-blocked: 1` tells the reader to offer loading it. Inline `cid:` images point at absolute attachment URLs built from the request's `Host` and `X-Forwarded-Proto`.
- The web app shows `/body` in an iframe with `sandbox="allow-same-origin allow-popups allow-popups-to-escape-sandbox"` and no `allow-scripts`; the document's own CSP forbids scripts as well.
- Attachments are served inline only for images and PDF; everything else, including HTML and SVG, is `application/octet-stream` with `Content-Disposition: attachment`. Every download has `X-Content-Type-Options: nosniff` and `Content-Security-Policy: sandbox`.
- File names in `Content-Disposition` are cleaned and sent both as ASCII and as RFC 5987 UTF-8.

## Not yet

Every signed-in user can read every archive: per-archive grants, roles beyond the stored one, passkeys, OIDC and API tokens come later. Write endpoints (accounts, sync, retention, export) come with the milestones in [PLAN.md](../../../docs/PLAN.md).
