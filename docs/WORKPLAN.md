# Work plan: milestones 1 and 2

A working checklist for building the first usable Holtmail: archive mail into a local or S3 blob store, search it, and read it in the web app. It is written so that work can resume from this file alone after a break or a context reset.

## How to resume

1. Read [AGENTS.md](../AGENTS.md), [PLAN.md](PLAN.md) and [design.md](design.md).
2. Find the first unchecked step below and the latest entry in [Log](#log).
3. `git status` shows work in progress; finish and check it before starting the next step.
4. After each step: `mise run check` (or the cargo commands in AGENTS.md), tick the step, add a log line.

Rules for this run: no commits and no pushes until the maintainer reviews in the morning; each step stays in its own module directories so the work can be committed module by module afterwards. Never start the web app in a browser to check it; check types, lint, tests and builds.

## Decisions

| Topic | Decision | Why |
| --- | --- | --- |
| Ids | UUIDv7 newtypes in `holtmail-core` (`ArchiveId`, `AccountId`, `FolderId`, `MessageId`, `UserId`) | Time-sortable, generated without the database, the same in SQLite and Postgres |
| Time | `Timestamp`: Unix milliseconds UTC as `i64` in `holtmail-core` | Plain, portable, sortable; formatting happens at the edges |
| Dedup | A message is unique per archive by its `BlobId`; the same blob in another archive is a separate message row pointing at the same blob | Access is granted per archive, storage is shared |
| Locations | A message can be in many folders; `(folder, uid)` rows record where it was seen, with `first_seen_at`, `last_seen_at`, `removed_at` | Archive never forgets; Gmail labels and copies across folders cost one blob |
| Attachments | Extracted on ingest into their own blobs and rows; the message blob stays whole | Dedup of forwarded files; fast attachment views |
| Threads | `thread_id` assigned on ingest from `References`/`In-Reply-To` through a per-archive index of `Message-ID`s, falling back to a new thread | Works for imports in any order: a late parent adopts the existing thread |
| Store | One crate `holtmail-store` with traits by area (`ArchiveStore`, `MessageStore`, `SyncStore`, `UserStore`) and a `Store` supertrait; `store-sqlite` on `sqlx` with embedded migrations | Small traits, one composition object |
| Search | `index-tantivy` behind `holtmail-index`; `holtmail-query` parses `from: to: subject: has:attachment before: after: in: is:` and free text | Database stays metadata-only and fast |
| Ingest | `holtmail-ingest`: raw bytes → blob → parse → attachments → store → index, one function used by sync and import | One path, one set of tests |
| HTML | `holtmail-sanitize` (ammonia): no scripts, no forms, remote images replaced by placeholders unless allowed, `cid:` rewritten to API URLs | The reader shows mail in a sandboxed iframe on top of this |
| Secrets | `holtmail-secret` contract, `secret-local`: XChaCha20-Poly1305 with a key file created on first start | Account passwords and OAuth tokens never stored in clear |
| API | `holtmail-api` on axum with OpenAPI from `utoipa`; JSON, cursor pagination, raw and attachment downloads | Typed client for the web is generated from it |
| Server | `apps/server`, binary `holtmail`: `serve`, `import`, `sync`, config in `holtmail.toml` with env overrides | One binary |
| Web tooling | Bun workspaces, Vite, React 19, TypeScript, oxlint, oxfmt; Tailwind v4; shadcn on Radix; Motion; TanStack Router and Query; `openapi-typescript` + `openapi-fetch` | Same tools as Holtkeeper |
| IMAP | `source-imap` on `async-imap` or `imap-next` (pick after a spike), tested against an in-process fake server | Tests never touch the network |

## Lessons from Mail-Archiver

What its code and issues teach, and what Holtmail does instead. Apply these when building each module.

| Mail-Archiver | Holtmail |
| --- | --- |
| Original bytes optional and off by default; bodies cut at 500 KB / 1 MB for `tsvector`; export and restore rebuild messages from database fields | The original bytes are always the blob; export and restore stream them unchanged; the index is separate and never truncates what is stored |
| Attachments and raw messages as `bytea` in Postgres: OOM kills, slow storage stats, dedup crashing at 341k mails | Bytes in `BlobStore` only; storage stats are kept as counters, not computed by reading bodies |
| One row per `Message-ID` with a single folder column: Gmail labels and copies lose their folders | Message unique by content hash; a location row per folder and UID |
| Messages without `Message-ID` and subject silently skipped | Every message is archived; identity is the content hash |
| No index on `Message-ID` | `(archive_id, rfc_message_id)` indexed for threading and lookups |
| Date-window sync re-downloading 12 hours each run, one message per round trip | UID watermark with `UIDVALIDITY`, `CONDSTORE` when present, batched `UID FETCH` of size and headers first, then bodies only for unknown messages |
| One broken message stops the checkpoint forever | Failures are recorded per UID with a reason and retried later with backoff; the cursor moves on |
| Sent dates stored in the display time zone | UTC milliseconds everywhere, formatted only in the browser |
| `simple` full-text search, `OFFSET` paging and exact `COUNT(*)`, unindexed `from:` | Tantivy with per-field indexing, keyset pagination, approximate counts |
| Delete-from-server trusts a `Message-ID` match | Only after the stored blob's hash matches the bytes fetched from that UID, and never when the server's date answer looks wrong |
| Graph: date filters, JSON-built messages, attached emails dropped | Graph delta queries and the MIME `$value` of every message |
| Legacy unsalted password hashes; a user-to-account table as the whole access model | Argon2id or passkeys; roles and per-archive grants as in PLAN.md |

Requests its users keep making, worth planning for: attachment content search, folder allow-lists, archiving only mail older than N days, SMTP or journal ingestion, webhooks, tamper-evident archiving, bulk delete with review, PDF export.

## Steps

### Backend core

- [x] `holtmail-core`: ids, `Timestamp`, `Address` with docs and tests
- [x] `holtmail-store`: contract traits, types, `StoreError`, conformance suite
- [x] `store-sqlite`: migrations, implementation, passes conformance

### Ingest and search

- [x] `holtmail-sanitize`: safe HTML for the reader
- [x] `holtmail-index`: contract and conformance; `holtmail-query`: search language parser
- [x] `index-tantivy`: implementation, passes conformance
- [x] `holtmail-ingest`: one function from raw bytes to stored, indexed message; threading
- [x] `holtmail-import`: one crate (format readers, not a contract) for mbox, Maildir, `.eml`, zip and directory trees

### Server

- [x] `holtmail-api`: archives, folders, messages list with cursor, message detail, raw, attachments, search, OpenAPI JSON
- [x] `apps/server`: config, `holtmail serve`, `holtmail import <archive> <path>`, wiring

### Web

- [x] Workspace tooling: `package.json`, bun via mise, tsconfig, oxlint, oxfmt, `mise run check` covers web
- [x] `@holtmail/tokens`: color (light and dark), type, spacing, motion as CSS variables and TS constants
- [x] `@holtmail/ui`: shadcn components restyled (button, input, dialog, command, dropdown, tooltip, scroll area, separator, badge, kbd, skeleton)
- [x] `@holtmail/api-client`: generated types and a fetch client
- [x] `apps/web`: app shell with three panes, routes, theme switch
- [x] Message list: virtualized, grouped by day, keyboard navigation
- [x] Reader: thread view, sandboxed HTML, attachments, raw source
- [x] Search: query bar with operators, command palette (`⌘K`)

### Sync

- [x] `holtmail-secret` and `secret-local`
- [x] `holtmail-source`: contract and a fake source for tests
- [x] `source-imap`: folders, `UIDVALIDITY`, incremental by UID, `CONDSTORE` when available, Gmail labels
- [x] `holtmail-sync`: per-folder sync with resumable cursors, retries, never deletes from the archive
- [x] `holtmail sync` command and a scheduler in `serve`

### API gaps the web needs

- [x] OpenAPI: declare path params (`id`, `ordinal`), the `x-holtmail-remote-blocked` header, 400/404/500 responses; regenerate `packages/api-client/src/schema.ts` and drop local re-typing
- [x] `MessageSummary`: flags (unread, flagged), thread message count, folder ids
- [x] `MessageDetail`: `preview`, `in_reply_to`, `references`
- [x] `FolderDto`: unread count
- [ ] Attachment thumbnails endpoint (later: the reader shows image attachments full size for now)
- [x] Web: route-level code splitting (bundle 674 KB)

### Sign-in

- [x] `UserStore`, `holtmail-auth` (Argon2id, session cookies, one-time setup link, rate limit), API session endpoints and CSRF checks, `holtmail user add/password` (delegated to a helper)
- [x] Web: setup and sign-in screens, 401 handling

### Project

- [x] GitHub Actions CI running `mise run check`
- [x] Docs index and PLAN status updated

### Phase 3: everything from the interface (night of 2026-11-03)

The maintainer asked that everything be managed in the web app (the CLI stays) and that the lead keep going without asking, aiming for the most convenient and beautiful result. Roles: owner and admins manage the instance (archives, accounts, users); members read the archives granted to them.

- [x] API: `Mailboxes` hook in `holtmail-api` (implemented by the server): add IMAP account (checks sign-in first), sync now, upload import, account status; `POST /api/archives`, `POST /api/archives/{id}/accounts`, `POST /api/archives/{id}/imports`, `POST /api/accounts/{id}/sync`; accept `application/octet-stream` uploads
- [x] Server: `Mailboxes` implementation with one runner per account shared by the scheduler, uploads under `data_dir/uploads`
- [x] Store: rename and delete archive, update and delete account, update user (name, role), delete user; conformance + SQLite
- [x] Access: per-archive grants for members; owner/admin see everything; manage routes need owner/admin; `GET /api/archives` lists only what the user may read
- [x] API: users (list, create with password, change role, remove), profile (name, password), archive rename/delete, account edit/remove/pause
- [x] Web: onboarding when there are no archives (create archive → connect IMAP with provider presets, or drop a file), live progress
- [x] Web: settings screen (archives and accounts with status and "sync now", import, users and access, profile and password), reachable from the sidebar, user menu and ⌘K
- [x] Web: sidebar shows sync state per account; empty archive state offers to connect mail
- [x] Regenerate the client, docs for every touched module, `mise run check`, Docker smoke test

### Phase 4: remote images (2026-11-03)

- [x] `holtmail-sanitize`: `Remote::{Block, Proxy, Direct}`, saved copies always shown through the proxy prefix, `remote_images`, `remote_key`
- [x] `holtmail-fetch` contract (public addresses only, images only) with a no-network conformance suite; `holtmail-fetch-http` on reqwest with a resolver that drops private addresses
- [x] Store: `Archive.save_remote_images`, `RemoteImageStore` (copies shared by URL key, scans, links), orphan handling in `delete_archive`; migration 0004
- [x] `holtmail-remote`: saves each URL once, identical bytes once, failures recorded
- [x] API: `?remote=block|proxy|direct`, `/api/messages/{id}/remote/{key}` (only URLs the message links), `PATCH /api/archives/{id}` `save_remote_images`
- [x] Server: fetcher composed, saving after syncs and imports, at start and when turned on
- [x] Web: Reading settings (ask / through Holtmail / directly, with an IP warning), banner Show and Always show…, archive switch with a warning; `Switch` and `RadioGroup` in `@holtmail/ui`

## Log

- Plan written. Earlier work: `holtmail-blob`, `blob-fs`, `blob-s3`, `holtmail-mime` (committed and pushed).
- Mail-Archiver studied (lessons table above). `holtmail-core` done; `Address` moved from mime to core.
- Web foundation (tooling, tokens, ui, app shell) delegated to a background helper; check its report before touching `packages/` or `apps/web`.
- `holtmail-store` contract (archives, accounts, folders, messages, threads, locations, cursors, failures) and `store-sqlite` done; conformance passes. `BlobId` now serializes as hex.
- `holtmail-sanitize` done: ammonia rules for email layouts, remote content blocking, `cid:` rewriting, CSP document.
- Web foundation done by helper: bun workspace, `@holtmail/tokens` (`--hm-*` vars, Tailwind `@theme`), `@holtmail/ui` (Button, Input, Kbd, Badge, Separator, Tooltip, DropdownMenu, Dialog, Command, ScrollArea, Skeleton, ToggleGroup, MotionProvider), `apps/web` shell. `mise run check` now runs web checks too.
- `holtmail-index` contract + conformance, `holtmail-query` parser, `index-tantivy` (memory and disk) done.
- `holtmail-ingest` (blob → parse → attachments → store → location → index) and `holtmail-import` done. Import is one crate rather than a contract: formats are readers, not replaceable providers.
- `holtmail-api` (axum + utoipa, own DTOs, safe body/attachments) and `apps/server` (`serve`, `import`, `archives`, `openapi`; TOML config; fs or S3 blobs) done. Smoke-tested import → serve → search with curl. `packages/api-client/openapi.json` generated by `holtmail openapi`.
- Secrets moved to the Sync section: nothing stores credentials until IMAP accounts exist.
- Web mail views + api-client delegated to a background helper (owns apps/web, packages/api-client, may add to packages/ui). Lead works on secrets, source, IMAP, sync in crates/.
- `holtmail-secret` + `secret-local` (XChaCha20-Poly1305, key file 0600, context = account id) done.
- `holtmail-source` contract + `FakeSource`, `holtmail-sync` engine done (UID cursor, removals, renumbering, failures with retry cap, folder rules). Next: `source-imap` on async-imap with an in-process fake IMAP server for tests, then `holtmail account add-imap` / `holtmail sync` and a scheduler.
- `source-imap` done (async-imap + rustls/ring + webpki-roots; UTF-7 names; special-use roles; read-only; in-process fake IMAP server tests). `FolderRole::guess` moved into `holtmail-store` for import and IMAP.
- Server: `account add-imap` / `account list` / `sync`, scheduler in `serve` (`sync_interval_minutes`, default 15), `secret.key` in the data dir. Folder rules from `settings.folders.{include,exclude}`.
- `holtmail reindex` added (index is rebuildable). CI workflow `.github/workflows/ci.yml` runs `mise run check`.
- Sign-in backend delegated to a helper (owns crates/store, store-sqlite, new crates/auth, api, apps/server). Do not edit those until it reports. Web login screens come after both helpers finish.
- Dockerfile (bun web build → rust release build → debian slim, uid 10001, /data volume) and .dockerignore added; not built locally yet. Root README has a quick start.

- Mail UI done by helper: api-client (openapi-typescript + openapi-fetch + query factories), three-pane mail layout, virtualized list with day groups, thread reader in sandboxed iframe (no allow-scripts), search with operator hints, ⌘K palette, shortcuts. 66 web tests; check and knip clean. API gaps listed above.
- Sign-in backend done by helper: `UserStore` (+ migration 0002), `holtmail-auth` (Argon2id, BLAKE3-hashed session tokens, one-time setup token, rate limit 10/15 min with decoy hash), API session routes + CSRF guard (JSON content type, Origin/Sec-Fetch-Site), `holtmail user add/password/list`, `public_url`. 91 Rust tests. Roles stored, not enforced yet. Next: API gaps + regenerate client + web setup/sign-in screens (one helper).
- API gaps closed: path params, `/body` header and error answers in OpenAPI; `MessageStore::message_extras` (batched flags, thread size, folders) and folder `unread` in the store contract, conformance and SQLite; `MessageSummary` flags/thread_count/folder_ids, `MessageDetail` preview/in_reply_to/references, `FolderDto.unread`; client regenerated without path re-typing. Web: `/login` and `/setup` screens, session checks with setup/login redirects and a global 401 handler, user menu (theme, sign out), unread/flag/thread marks in the list, folder unread counts, quoted reply. Thumbnails and code splitting still open.
- Lead review: full `mise run check` green (92 Rust tests, 79 web tests, knip clean). Flagged star switched to `fg-muted` to keep one accent. README quick start mentions the setup link. Docker image builds (195 MB) and runs: `/api/setup` answers, the web app is served, the setup link is logged.

## Next (for the morning)

- Phase 3 (everything from the interface) is uncommitted. Suggested commits: store, store-sqlite, auth, source-imap, api, server, api-client, web, docs.
- Look at the first-run screen and settings in a browser yourself: they are verified by types, lint, tests, builds and curl runs against the real server, not by eye.
- Then: per-archive grants and role enforcement, XOAUTH2 for Gmail/Microsoft, Gmail labels via `X-GM-LABELS`, CONDSTORE flag sync, attachment thumbnails, code splitting, export and restore.
- Phase 3 backend done: `Mailboxes` hook (server `Runner`: one run per account, status in memory, uploads in `data_dir/uploads`), store removal/user edits/grants (+ migration 0003, conformance), access middleware (members read granted archives only, 404 otherwise; managers do everything), users/profile/password/archive/account endpoints. 18 API tests. Next: web onboarding and settings.
- Phase 3 web done: `/welcome` first run (archive name → connect IMAP with provider presets or upload files → live progress), `/settings` (archives with sources, status, sync now, pause, password, remove, rename/delete archive; people with roles and archive access; profile and password), sidebar source status, empty-archive state, palette actions, live refresh while syncing, lazy route chunks (main 356 KB). End-to-end smoke test with curl: setup → archive → upload mbox → search → member sees nothing and gets 403 on writes → delete archive removes blobs.
- Lead review: `mise run check` green (98 Rust tests, 94 web tests, knip clean). Docker image rebuilt; setup → archive → upload → import works in the container.
- Phase 4 (remote images) done: proxy, three reading modes with an IP warning, per-archive saving with dedup by URL and by content. End-to-end with the real binary: blocked by default, proxied URLs rewritten, a loopback image refused (404), saving records "not a public address" and "host not found". Fetching from the real internet was not exercised; tests use fakes.

