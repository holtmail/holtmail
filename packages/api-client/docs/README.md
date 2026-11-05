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

`Archive`, `Account`, `AccountStatus`, `Person`, `Role`, `RemoteMode` (`block`, `proxy`, `direct`), `Task`, `TaskLine`, `ImageProxy`, `RemoteImageJob` (`older`, `failed`), `ImapConnection`, `ConnectionRequest`, `JmapConnection` (`url`, `username`, `auth`, `exclude`), `JmapConnectionRequest`, `JmapAuth` (`password`, `token`), `SyncSchedule`, `OAuthStart` (`authorize_url`, `state`, `needs_paste`, `thunderbird_client`, `expires_at`), `OAuthProvider` (`google`, `microsoft`), `OAuthKind` (`imap`, `graph`), the request bodies (`ArchiveUpdateRequest`, `ImapAccountRequest`, `JmapAccountRequest`, `AccountUpdateRequest` (with `jmap` for JMAP accounts), `OAuthStartRequest` (with `provider` narrowed to `OAuthProvider` and `kind` to `OAuthKind`), `NewUserRequest`, `UserUpdateRequest`, `ProfileRequest`, `PasswordRequest`), `Folder`, `Label` (`name`, `messages`, `unread`), `MessageSummary` (with its sorted `labels` and, in search results, `attachment_match`), `MessagePage`, `SearchResponse`, `MessageDetail`, `Thread`, `ExportFormat` (`mbox`, `zip`), `ExportScope` (`folder`, `label`, `q`), `Address`, `Attachment` (with `url` and `thumbnail_url` for images), `MessageLocation`, `User`, `LoginRequest`, `SetupRequest`: aliases of the generated schemas. Times are Unix milliseconds.

### Errors

Every failed request throws `ApiError` with `status` (HTTP status), `message`, which is the API's `{ "error": "..." }` text when the response has one and the status text otherwise, and `retryAfter`, the `Retry-After` seconds when the response has them (a `429` from sign-in). `isUnauthorized(error)` is true for a `401`. Search answers `400` with the query parser's message; `searchInfiniteQuery` does not retry any `4xx`.

### Query options

| Factory                                    | Key                                                  | Data                                                                                                                   |
| ------------------------------------------ | ---------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| `sessionQuery`                             | `['session']`                                        | The signed-in `User`; throws a `401` `ApiError` otherwise; never retried or stale                                      |
| `setupStatusQuery`                         | `['setup']`                                          | `{ needed }`, whether the instance still needs its owner                                                               |
| `archivesQuery`                            | `['archives']`                                       | `Archive[]`                                                                                                            |
| `accountsQuery(archiveId)`                 | `['archives', id, 'accounts']`                       | `Account[]`                                                                                                            |
| `foldersQuery(archiveId)`                  | `['archives', id, 'folders']`                        | `Folder[]`                                                                                                             |
| `labelsQuery(archiveId)`                   | `['archives', id, 'labels']`                         | `Label[]` carried by at least one message, by name; empty without Gmail labels                                         |
| `messagesInfiniteQuery(archiveId, f?, l?)` | `['archives', id, 'messages', f \| null, l \| null]` | Pages of `MessagePage`, newest first, in folder `f` and with label `l` when given; the page param is the `next` cursor |
| `searchInfiniteQuery(archiveId, q)`        | `['archives', id, 'search', q]`                      | Pages of `SearchResponse`; the page param is `next_offset`                                                             |
| `messageQuery(id)`                         | `['messages', id]`                                   | `MessageDetail`                                                                                                        |
| `threadQuery(id)`                          | `['threads', id]`                                    | `Thread`, oldest message first                                                                                         |
| `bodyQuery(id, remote, saved = true)`      | `['messages', id, 'body', remote, saved]`            | `{ html, remoteBlocked, savedShown }`; `saved: false` shows originals; never stale                                     |
| `tasksQuery`                               | `['tasks']`                                          | `Task[]`; refetched every second while any runs, every 10 seconds otherwise                                            |
| `syncScheduleQuery`                        | `['sync-schedule']`                                  | `SyncSchedule`: minutes for accounts without their own, and `from_config`                                              |
| `imageProxyQuery`                          | `['image-proxy']`                                    | `ImageProxy`                                                                                                           |
| `rawQuery(id)`                             | `['messages', id, 'raw']`                            | The original message as text, never stale                                                                              |

Pages hold 100 messages. Spread a factory into `useQuery` / `useInfiniteQuery` to add options such as `enabled`.

### Helpers

| Export                                        | Guarantee                                                                                                                               |
| --------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `bodyUrl(id, remote = 'block', saved = true)` | `/api/messages/{id}/body` with `remote`, and `saved=false` when `saved` is false                                                        |
| `rawUrl(id)`                                  | `/api/messages/{id}/raw`, the original bytes as a `.eml` download                                                                       |
| `exportUrl(archiveId, format, scope?)`        | `/api/archives/{id}/export?format=…`, narrowed by the first of `folder`, `label` or a non-blank `q` in `scope`; a download, not a query |
| `fetchBody(id, remote, sig?)`                 | The sanitized HTML document and whether `x-holtmail-remote-blocked` was `1`                                                             |
| `fetchRaw(id, sig?)`                          | The raw message as text                                                                                                                 |
| `client`                                      | The typed `openapi-fetch` client, for calls the factories do not cover                                                                  |
| `login({ email, password })`                  | `POST /api/login`; the signed-in `User`                                                                                                 |
| `setup({ token, email, name, password })`     | `POST /api/setup`; the new owner, signed in                                                                                             |
| `logout()`                                    | `POST /api/logout` with `{}`; idempotent                                                                                                |

### Changes

Each resolves with the changed record (or nothing for a delete) and throws `ApiError` otherwise. They do not touch the cache; callers invalidate what they show.

| Export                                                                                          | Request                                                                                                                                                                                                                                                                                                                                                                     |
| ----------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `createArchive(name)`, `updateArchive(id, { name?, save_remote_images? })`, `deleteArchive(id)` | `POST /api/archives`, `PATCH` / `DELETE /api/archives/{id}`                                                                                                                                                                                                                                                                                                                 |
| `addImapAccount(archiveId, request)`                                                            | `POST /api/archives/{id}/accounts`; the server signs in first                                                                                                                                                                                                                                                                                                               |
| `addJmapAccount(archiveId, request)`                                                            | `POST /api/archives/{id}/accounts/jmap`; the server signs in first                                                                                                                                                                                                                                                                                                          |
| `updateAccount(id, request)`, `deleteAccount(id)`, `syncAccount(id)`                            | `PATCH` / `DELETE /api/accounts/{id}`, `POST /api/accounts/{id}/sync`                                                                                                                                                                                                                                                                                                       |
| `setSyncSchedule(minutes)`                                                                      | `PUT /api/sync-schedule`; resolves with the new `SyncSchedule`                                                                                                                                                                                                                                                                                                              |
| `startOAuth({ provider, email, archive_id \| account_id, name?, kind?, return_to? })`           | `POST /api/oauth/start`; resolves with an `OAuthStart`. `archive_id` adds an account, read over IMAP or, with `kind: 'graph'` and `microsoft`, through Microsoft Graph; `account_id` signs an existing IMAP or Graph account in again (or moves an IMAP one from a password to OAuth), keeping its kind; `return_to` is the app path the callback sends the browser back to |
| `finishOAuth(pastedUrl)`                                                                        | `POST /api/oauth/finish` with the address the provider left the browser on (when `needs_paste`); resolves with the new or updated `Account`, whose first sync has started. A bad paste, an unknown, expired or used sign-in, refused access or a token the mail server refuses is a `400` with the reason                                                                   |
| `uploadImport(archiveId, file, { onProgress, signal })`                                         | `POST /api/archives/{id}/imports?name=` with the file as `application/octet-stream` over `XMLHttpRequest`, so `onProgress(loaded, total)` reports the upload; resolves with the import account                                                                                                                                                                              |
| `startRemoteImages(archiveId, job)`, `cancelTask(id)`                                           | `POST /api/archives/{id}/remote-images` (resolves with the task), `POST /api/tasks/{id}/cancel`                                                                                                                                                                                                                                                                             |
| `setImageProxy(proxy)`                                                                          | `PUT /api/image-proxy` with a URL or `null`; resolves with the new `ImageProxy`                                                                                                                                                                                                                                                                                             |
| `createUser`, `updateUser(id, request)`, `deleteUser(id)`                                       | `/api/users`                                                                                                                                                                                                                                                                                                                                                                |
| `updateProfile(request)`, `changePassword(request)`                                             | `PATCH /api/me`, `POST /api/me/password`; both resolve with the signed-in `User`                                                                                                                                                                                                                                                                                            |

When `needs_paste` is false the provider returns the browser to `GET /api/oauth/callback`, which finishes on its own and redirects to `return_to` with `?oauth=done&account=<id>` or `?oauth_error=<reason>`; the app reads those itself. `Account.oauth_provider` is `google` or `microsoft` for an OAuth account, and `needs_sign_in` is true once the provider stopped accepting its sign-in: it does not sync, and `syncAccount` and a password change answer `400`.

`accountsQuery` refetches every 2 seconds while any account syncs or imports and every minute otherwise. `usersQuery` (`['users']`) lists people with their grants.

Every `POST` sends `Content-Type: application/json`, which the server requires of every write.

## Tests

`bun test packages/api-client` covers the URL helpers, error handling, `Retry-After` and the sign-in requests with a stubbed `fetch`.
