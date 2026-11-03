# @holtmail/api-client

The only way the web app talks to the server. Types are generated from the server's OpenAPI description, requests go through `openapi-fetch`, and every read is exposed as a TanStack Query option factory so components share one cache. The package exports TypeScript source; the app bundles it.

## Generating types

`openapi.json` is written by the Rust server (`holtmail openapi`) and is never edited by hand. After it changes:

```sh
cargo run -q -p holtmail-server -- openapi > packages/api-client/openapi.json
cd packages/api-client && bun run generate
```

`bun run fmt` at the root then formats `openapi.json` like every other JSON file.

This runs `openapi-typescript` into `src/schema.ts` and formats it with `oxfmt`. The output is committed. `openapi-typescript` needs the TypeScript 5 compiler API, so the package pins `typescript@5` as a dev dependency next to the workspace's TypeScript 7.

## API

All requests are same-origin: paths start with `/api`, which the Vite dev server proxies and the `holtmail` binary serves.

### Types

`Archive`, `Account`, `AccountStatus`, `Person`, `Role`, the request bodies (`ImapAccountRequest`, `AccountUpdateRequest`, `NewUserRequest`, `UserUpdateRequest`, `ProfileRequest`, `PasswordRequest`), `Folder`, `MessageSummary`, `MessagePage`, `SearchResponse`, `MessageDetail`, `Thread`, `Address`, `Attachment`, `MessageLocation`, `User`, `LoginRequest`, `SetupRequest`: aliases of the generated schemas. Times are Unix milliseconds.

### Errors

Every failed request throws `ApiError` with `status` (HTTP status), `message`, which is the API's `{ "error": "..." }` text when the response has one and the status text otherwise, and `retryAfter`, the `Retry-After` seconds when the response has them (a `429` from sign-in). `isUnauthorized(error)` is true for a `401`. Search answers `400` with the query parser's message; `searchInfiniteQuery` does not retry any `4xx`.

### Query options

| Factory                                | Key                                       | Data                                                                              |
| -------------------------------------- | ----------------------------------------- | --------------------------------------------------------------------------------- |
| `sessionQuery`                         | `['session']`                             | The signed-in `User`; throws a `401` `ApiError` otherwise; never retried or stale |
| `setupStatusQuery`                     | `['setup']`                               | `{ needed }`, whether the instance still needs its owner                          |
| `archivesQuery`                        | `['archives']`                            | `Archive[]`                                                                       |
| `accountsQuery(archiveId)`             | `['archives', id, 'accounts']`            | `Account[]`                                                                       |
| `foldersQuery(archiveId)`              | `['archives', id, 'folders']`             | `Folder[]`                                                                        |
| `messagesInfiniteQuery(archiveId, f?)` | `['archives', id, 'messages', f \| null]` | Pages of `MessagePage`, newest first; the page param is the `next` cursor         |
| `searchInfiniteQuery(archiveId, q)`    | `['archives', id, 'search', q]`           | Pages of `SearchResponse`; the page param is `next_offset`                        |
| `messageQuery(id)`                     | `['messages', id]`                        | `MessageDetail`                                                                   |
| `threadQuery(id)`                      | `['threads', id]`                         | `Thread`, oldest message first                                                    |
| `bodyQuery(id, remote)`                | `['messages', id, 'body', remote]`        | `{ html, remoteBlocked }`, never stale                                            |
| `rawQuery(id)`                         | `['messages', id, 'raw']`                 | The original message as text, never stale                                         |

Pages hold 100 messages. Spread a factory into `useQuery` / `useInfiniteQuery` to add options such as `enabled`.

### Helpers

| Export                                    | Guarantee                                                                   |
| ----------------------------------------- | --------------------------------------------------------------------------- |
| `bodyUrl(id, remote = false)`             | `/api/messages/{id}/body`, with `?remote=true` when remote content may load |
| `rawUrl(id)`                              | `/api/messages/{id}/raw`, the original bytes as a `.eml` download           |
| `fetchBody(id, remote, sig?)`             | The sanitized HTML document and whether `x-holtmail-remote-blocked` was `1` |
| `fetchRaw(id, sig?)`                      | The raw message as text                                                     |
| `client`                                  | The typed `openapi-fetch` client, for calls the factories do not cover      |
| `login({ email, password })`              | `POST /api/login`; the signed-in `User`                                     |
| `setup({ token, email, name, password })` | `POST /api/setup`; the new owner, signed in                                 |
| `logout()`                                | `POST /api/logout` with `{}`; idempotent                                    |

### Changes

Each resolves with the changed record (or nothing for a delete) and throws `ApiError` otherwise. They do not touch the cache; callers invalidate what they show.

| Export                                                                | Request                                                                                                                                                                                        |
| --------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `createArchive(name)`, `renameArchive(id, name)`, `deleteArchive(id)` | `POST /api/archives`, `PATCH` / `DELETE /api/archives/{id}`                                                                                                                                    |
| `addImapAccount(archiveId, request)`                                  | `POST /api/archives/{id}/accounts`; the server signs in first                                                                                                                                  |
| `updateAccount(id, request)`, `deleteAccount(id)`, `syncAccount(id)`  | `PATCH` / `DELETE /api/accounts/{id}`, `POST /api/accounts/{id}/sync`                                                                                                                          |
| `uploadImport(archiveId, file, { onProgress, signal })`               | `POST /api/archives/{id}/imports?name=` with the file as `application/octet-stream` over `XMLHttpRequest`, so `onProgress(loaded, total)` reports the upload; resolves with the import account |
| `createUser`, `updateUser(id, request)`, `deleteUser(id)`             | `/api/users`                                                                                                                                                                                   |
| `updateProfile(request)`, `changePassword(request)`                   | `PATCH /api/me`, `POST /api/me/password`; both resolve with the signed-in `User`                                                                                                               |

`accountsQuery` refetches every 2 seconds while any account syncs or imports and every minute otherwise. `usersQuery` (`['users']`) lists people with their grants.

Every `POST` sends `Content-Type: application/json`, which the server requires of every write.

## Tests

`bun test packages/api-client` covers the URL helpers, error handling, `Retry-After` and the sign-in requests with a stubbed `fetch`.
