# Work plan

A working checklist for building the first usable Holtmail: archive mail into a local or S3 blob store, search it, and read it in the web app. It is written so that work can resume from this file alone after a break or a context reset.

## How to resume

1. Read [AGENTS.md](../AGENTS.md), [PLAN.md](PLAN.md) and [design.md](design.md).
2. Find the first unchecked step below and the latest entry in [Log](#log).
3. `git status` shows work in progress; finish and check it before starting the next step.
4. After each step: `mise run check` (or the cargo commands in AGENTS.md), tick the step, add a log line.

Rules: no commits and no pushes without the maintainer's word; each step stays in its own module directories so the work can be committed module by module afterwards. Never start the web app in a browser to check it; check types, lint, tests and builds.

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
| OAuth | `holtmail-oauth` (presets, PKCE, redirect parsing, token answers) with the HTTP call behind its `TokenEndpoint` contract, implemented by `oauth-http`; how an account signs in lives in its settings JSON (`oauth: { provider, needs_sign_in }`) and the refresh token in `sealed_secret`, so no migration; access tokens and waiting sign-ins only in server memory | Same contract/implementation split as fetch; tests need no network; the store stays ignorant of credentials |
| IMAP | `source-imap` on `async-imap` or `imap-next` (pick after a spike), tested against an in-process fake server | Tests never touch the network |
| Labels | Gmail labels are rows per archive (`labels`, `message_labels`), set from `X-GM-LABELS` of `[Gmail]/All Mail`; label folders are skipped while All Mail syncs; system labels read `Inbox`, `Sent`, `Starred`, `Important`, `Drafts`, `Spam`, `Trash` | One download per message instead of one per label; labels are many-to-many and filterable, folders stay what the server has |
| Attachments, derived | `holtmail-extract` contract + `extract-basic` (lopdf, zip + quick-xml, mail-parser's HTML to text); `holtmail-thumbnail` is a plain library (one pure function, nothing to swap); both results are blobs recorded in `DerivedStore` (`derived` table: source blob, kind, blob or "nothing"); text extracted on ingest once per attachment blob, thumbnails on first request | Raw bytes stay the truth; dedup across forwards; reindex and flag changes reuse cached text; failures recorded once |
| Export | `holtmail-export`, a library (formats are writers, not providers) over the store, blob and index contracts; mboxrd or a zip of `.eml` files written as a stream (`zip` stream writer, data descriptors) straight into the HTTP response, no temp files or background task; members export what they can read | Original bytes unchanged, about one message in memory, works on S3; a download needs no job queue or cleanup |
| Backup | `holtmail backup` writes one tar (`manifest.json` first, a `VACUUM INTO` snapshot, blobs the snapshot references through `InventoryStore`, `secret.key` only with `--include-secret-key`); `restore` only into an empty data directory, every blob checked against its hash, then a full reindex | One file that streams to a pipe; consistent while `serve` runs; works for S3 blobs; the key stays out unless asked |
| Task history | `TaskStore` in the store contract (`tasks`, `task_log`, migration 0009); the server's registry serves from memory and writes behind at most once a second (and on shutdown); 200 finished tasks kept; ids are UUIDv7 strings; tasks left `running` become `interrupted` at start | One process writes, no write per progress tick, ids stay unique across restarts, a crash loses at most a second of progress |
| Last runs | `SyncStore::set_last_run`/`last_runs` (`account_runs`), loaded into the runner at start; the schedule counts from the stored run; an interrupted sync is due at once | The sidebar, settings and "next sync" read the same after a restart |
| Minimum age | `min_age_days` in the account's settings JSON, `SyncOptions::min_age` in `holtmail-sync`; a folder stops at its first too-young new message so the UID cursor never passes unarchived mail; received time, else `Date`, else archived | Nothing is skipped for good; no contract change to sources; at most one batch re-fetched per folder and sync |
| JMAP | `source-jmap` on reqwest behind the unchanged-shape source contract: mailboxes are folders with location rows (an email in several mailboxes is one blob in several folders, like an IMAP copy); UID = `receivedAt` seconds << 22 \| 22 bits of FNV-1a of the email id, `UIDVALIDITY` from the mailbox id; new `FolderStatus::uids_ascend` (`false` for JMAP) makes the sync fetch every UID without a live location instead of those above the cursor; no mod-sequences, so flags are re-read daily; `Email/changes` not used; settings `{ url, username, auth: password\|token }`, the token sealed like a password and passed as `Credentials::AccessToken` (bearer), so `Credentials` is unchanged; https only, http just for loopback | Keeps `holtmail-sync`, the store and its cursors as they are (one small contract field instead of a generic cursor); stable UIDs without storing a map; a message moved into a mailbox later is still found; received-time order keeps the minimum-age stop correct |
| Graph | `source-graph` on reqwest, OAuth only (Microsoft, `Access::Graph`: `Mail.Read User.Read offline_access`; the account kind decides the scopes of the sign-in and of every refresh); folders from `mailFolders` with `childFolders` and hidden ones, search folders skipped, roles from the six well-known names; UIDs derived as for JMAP from `receivedDateTime` and the immutable id (`Prefer: IdType="ImmutableId"`), `uids_ascend: false`; each folder listed with a fresh delta query and its delta link kept as the opaque `FolderCursor::state`, which the next sync asks through the new `SourceSession::state`/`unchanged_since` (defaults keep IMAP and JMAP as they were): no changes means the folder is skipped, any change means a full delta listing; the state is stored only after a sync that left nothing deferred and no failure to retry, and dropped before any other sync; bytes from `$value` only; `429`/`503`/`504` waited out with `Retry-After`; created through the Phase 7 routes with `kind: graph` and `holtmail account add-graph` | Applying delta changes themselves would need every archived message's Graph id (retries, deferred messages and `@removed` entries carry or need only the id), which the store does not keep; skipping quiet folders gets most of the saving with no store change, and the `state` column already existed |
| Flag changes | `CONDSTORE`: `CHANGEDSINCE` the cursor's `highest_modseq` on every sync; without it, every archived UID's flags once a day in batches of 1000 UIDs (`flags_checked_at` in the cursor); changes rewrite the location's flags and labels and rebuild the index document | Cheap when the server helps, bounded when it does not; search stays true to the server |

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
- [x] Attachment thumbnails endpoint: moved to Phase 9
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

### Phase 5: image proxy, tasks, older mail on request (2026-11-03)

- [x] `fetch-http`: outbound proxy (`http`, `https`, `socks5`, `socks5h`); `proxy_address`
- [x] Store: `SettingsStore`, `save_remote_images_since`, scans by age, totals with failures, failed copies; migration 0005
- [x] `holtmail-remote`: `Scope::New` / `Scope::Older`, progress, cancel, `retry_failed`
- [x] API: tasks, cancel, `remote-images` jobs, image proxy, `?saved=false` and `/live/{key}` for originals
- [x] Server: task registry in memory, syncs, imports and image work as tasks with logs; proxy sealed in settings, config wins, swapped at runtime
- [x] Web: Tasks and Network settings, archive panel with older mail and retry, originals in the reader, running tasks in the sidebar

### Phase 6: sync schedule and editable connections (2026-11-03)

- [x] Server: scheduler checks every 30 s, per-account interval in its settings, server-wide interval from config, settings or 15 min; `HOLTMAIL_SYNC_INTERVAL`
- [x] Server: change server, port, security, user name and excluded folders, signed in first; password optional
- [x] API: `connection`, `sync_interval_minutes` and `next_sync_at` on accounts (connection hidden from members), `PATCH` with `connection` and interval, `/api/sync-schedule`
- [x] Web: Edit connection dialog, Check for new mail submenu per source, next sync in the row, server-wide interval above the archives

### Phase 7: Gmail and Microsoft sign-in (OAuth)

Microsoft turned off password sign-in for IMAP, and Gmail only accepts app passwords. Gmail, Google Workspace, Outlook.com, Hotmail and Microsoft 365 accounts sign in with OAuth and IMAP `AUTHENTICATE XOAUTH2`.

The default client is the public OAuth client Thunderbird ships for its desktop and mobile apps. Its client ids, and the Google client secret, are public in `mailnews/base/src/OAuth2Providers.sys.mjs` in comm-central. An installed-app client cannot redirect to a self-hosted address, so the flow is:

1. Holtmail opens the provider's sign-in page with PKCE and `state`. The redirect goes to the loopback address the client is registered for:
   - Google: `http://localhost` or `http://127.0.0.1`, on any port.
   - Microsoft: `http://localhost` or `https://localhost`. Check this against the live registration; Thunderbird bug 2036505 shows Microsoft switching between the two. Current comm-central sets no `redirectionEndpoint` for either provider, so both use the default `http://127.0.0.1` (plus a port); Holtmail uses `http://127.0.0.1`, overridable with `redirect_url`.
2. When Holtmail itself runs on that loopback address, its callback route catches the code directly.
3. Otherwise the browser lands on a page that does not load. The user pastes that page's address into Holtmail, which reads `code` and `state` from it.

The owner can set their own client per provider in `[oauth.google]` or `[oauth.microsoft]`, with `client_id`, `client_secret` and a `redirect_url` that points at Holtmail's callback. With their own client there is no paste step.

Risks to state in the docs and the UI:

- The consent screen names Thunderbird.
- The provider can block or rate-limit a borrowed client at any time.
- Whether this fits the provider's terms is the user's call.

Use device code flow for Microsoft if Thunderbird's registration allows it (to check). Not done: checking it needs a live request to Microsoft; the paste flow covers Microsoft for now.

- [x] `holtmail-oauth`:
  - provider presets for Google and Microsoft: issuer, endpoints, and the scopes `https://mail.google.com/` and `https://outlook.office.com/IMAP.AccessAsUser.All offline_access`;
  - PKCE, the authorize URL, code exchange and token refresh;
  - reading `code` and `state` from a pasted address;
  - no network in tests (a fake token endpoint).
- [x] `holtmail-source`: credentials are either `Password` or `AccessToken`.
- [x] `source-imap`: `AUTHENTICATE XOAUTH2` and its error answer, tested against the fake IMAP server.
- [x] Store: the account's auth kind, and the refresh token sealed with `secret.key` like passwords; nothing about tokens goes into logs.
- [x] Server:
  - `[oauth.*]` in the config, with Thunderbird's clients as the default;
  - refresh the token before each sync;
  - a revoked token marks the account "sign in again" instead of retrying forever.
- [x] API: start sign-in (returns the authorize URL and a short-lived state), finish it from a pasted address or the callback, and sign in again for an existing account.
- [x] Web:
  - the Gmail and Outlook presets use "Sign in with Google" and "Sign in with Microsoft";
  - a paste step with plain instructions;
  - "Sign in again" on an account;
  - password and app-password sign-in stay available.
- [x] Docs: provider setup, registering your own client, and the risks above.

### Phase 8: Gmail labels and flag changes

- [x] `source-imap`: on Gmail, read `X-GM-LABELS` and `X-GM-MSGID`, sync `[Gmail]/All Mail` once and attach labels instead of fetching every label folder.
- [x] Store and API: labels per message, and a label filter (`label:`) in search and in the sidebar.
- [x] `source-imap` and `holtmail-sync`: use `CONDSTORE` and `CHANGEDSINCE` to pick up read and flagged changes made after archiving; scan flags instead on servers without them.
- [x] Web: labels in the list and in the reader.

### Phase 9: attachments

- [x] An attachment thumbnails endpoint: images first, cached as derived blobs that can be rebuilt.
- [x] Text extraction from attachments (PDF, DOCX, XLSX, ODT, plain text) into the index's `attachment_text`:
  - behind a contract, so the extractor can be swapped;
  - `holtmail reindex` fills it for old mail.
- [x] Web: thumbnails in the reader, and attachment matches marked in search results.

### Phase 10: export, backup and restore

- [x] Export an archive, a folder or a search result as mbox or as a zip of `.eml` files:
  - streamed from the stored bytes, unchanged;
  - available from the web app and as `holtmail export`.
- [x] Instance backup:
  - a database snapshot, the blobs, and a warning about `secret.key`;
  - `holtmail backup` and `holtmail restore`, with a check that every blob hash matches.
- [x] Docs: what to back up and how to move an instance.

### Phase 11: state that survives a restart

- [x] Store the last sync result per account and the recent task list with logs (bounded), so Tasks and the sidebar show the same after a restart.
- [x] Tasks interrupted by a restart show as interrupted; remote image work resumes where it stopped.

### Phase 12: checks against the real world

The full list lives in [VERIFY.md](VERIFY.md).

- [ ] Sync real IMAP accounts (Gmail with OAuth, Outlook with OAuth, one plain IMAP host) and change a connection.
- [ ] Fetch remote images from the internet directly, through an HTTP proxy and through a SOCKS proxy.
- [ ] The maintainer walks through the web app by eye: first run, connecting mail, reading, search, settings, tasks, network, and a message with images.

### Contacts and calendars

The design is in [PIM.md](PIM.md); each phase follows its order of work.

#### Phase 13: model, storage and file import

- [ ] `holtmail-pim`: JSContact and JSCalendar views over `calcard`, field diff between two versions, identity keys (event `UID` + `RECURRENCE-ID`, normalized emails and E.164 phones)
- [ ] Store: collections, items, item versions (raw blob, ETag, seen, removed), derived `pim_index`; conformance + SQLite
- [ ] Import `.vcf` and `.ics` files and Google Takeout contacts and calendars as an import source

#### Phase 14: events from invitations in mail

- [ ] Ingest: `text/calendar` parts (iMIP `REQUEST`, `REPLY`, `CANCEL`) become event versions with the message as their source; old mail through a background pass
- [ ] API and reader: "this invitation is in your calendar" with its history

#### Phase 15: CardDAV and CalDAV

- [ ] `holtmail-collection` contract (collections, changes since a token, fetch, list versions for recovery), fake and conformance suite
- [ ] `collection-dav`: discovery (RFC 6764, principal, home sets), `sync-collection`, `multiget`, ETag listing; password and OAuth bearer
- [ ] `holtmail-pim-sync`: versions, removals, token recovery; scheduler and tasks; "also archive contacts and calendars" per account, Google CardDAV and CalDAV scopes

#### Phase 16: contacts and people

- [ ] Derived people (by email or phone, with the reason), people from mail, links to correspondence
- [ ] API and web: contacts page, person page with per-field sources, correspondence, meetings and the timeline

#### Phase 17: calendar

- [ ] `holtmail-recur`: recurrence expansion with time zones; derived occurrences for a window
- [ ] Grouping copies by `UID`, newest by `SEQUENCE`/`DTSTAMP`, differences between copies
- [ ] API and web: month, week and agenda views, event page with copies, attendees, mail thread and timeline; index `is:event`, `is:contact`, `with:`

#### Phase 18: Microsoft Graph contacts and calendars

- [ ] `collection-graph`: contacts and events with delta, mapped into JSContact and JSCalendar, raw JSON kept; Graph scopes `Contacts.Read`, `Calendars.Read`

#### Phase 19: JMAP contacts and calendars

- [ ] `collection-jmap`: JMAP for Contacts (RFC 9610); calendars where the server offers them

#### Phase 20: people by hand, differences, export

- [ ] Manual split and join of people (the only stored state), "has differences" filter
- [ ] Export `.vcf` and `.ics` of the latest versions, or of every version as a zip

### Later

- [x] `holtmail account edit` in the CLI (connection, interval, pause)
- [x] JMAP source (`source-jmap`)
- [x] Microsoft Graph source (`source-graph`) with delta queries and the MIME `$value` of each message
- [ ] Ideas from Mail-Archiver users (see above):
  - archive only mail older than N days (done: `min_age_days` per account);
  - SMTP or journal ingestion;
  - webhooks;
  - tamper-evident archiving;
  - bulk delete with review;
  - PDF export.

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
- Phase 3 backend done: `Mailboxes` hook (server `Runner`: one run per account, status in memory, uploads in `data_dir/uploads`), store removal/user edits/grants (+ migration 0003, conformance), access middleware (members read granted archives only, 404 otherwise; managers do everything), users/profile/password/archive/account endpoints. 18 API tests. Next: web onboarding and settings.
- Phase 3 web done: `/welcome` first run (archive name → connect IMAP with provider presets or upload files → live progress), `/settings` (archives with sources, status, sync now, pause, password, remove, rename/delete archive; people with roles and archive access; profile and password), sidebar source status, empty-archive state, palette actions, live refresh while syncing, lazy route chunks (main 356 KB). End-to-end smoke test with curl: setup → archive → upload mbox → search → member sees nothing and gets 403 on writes → delete archive removes blobs.
- Lead review: `mise run check` green (98 Rust tests, 94 web tests, knip clean). Docker image rebuilt; setup → archive → upload → import works in the container.
- Phase 4 (remote images) done: proxy, three reading modes with an IP warning, per-archive saving with dedup by URL and by content. End-to-end with the real binary: blocked by default, proxied URLs rewritten, a loopback image refused (404), saving records "not a public address" and "host not found". Fetching from the real internet was not exercised; tests use fakes.
- Phase 5 done: images of older mail only on request, retry of failed images, originals on request even with saved copies, outbound image proxy (UI or config, sealed), a task list with progress and logs. End-to-end with the real binary: enabling starts nothing, older mail runs as a task with a log, retry runs, an unreachable proxy is refused, a reachable one is stored without its password in the clear.
- Phase 6 done: per-account and server-wide sync intervals from the web app, next sync shown, connections editable with a sign-in check. Checked with the real binary: intervals change the next sync time, an unreachable new server is refused and nothing is stored, failed syncs show in Tasks.
- Phases 7-12 planned from the list of what is left: OAuth sign-in with Thunderbird's public clients by default, Gmail labels and flag changes, attachments, export and backup, durable task state, checks against real services.
- Phase 7 done: Gmail and Microsoft sign-in with OAuth. New `holtmail-oauth` (Thunderbird's public clients copied from comm-central, PKCE S256, authorize address with `login_hint` and Google's `access_type=offline&prompt=consent`, pasted-address reader, token answers with `invalid_grant` as `Revoked`) and `holtmail-oauth-http` (reqwest, https only, loopback http for tests, conformance suite). `holtmail-source` `Credentials::{Password, AccessToken}`; `source-imap` `AUTHENTICATE XOAUTH2` answering the error continuation with an empty line, tested on the fake server. No store change or migration: `settings.oauth` plus the sealed refresh token. Server: `[oauth.google]`/`[oauth.microsoft]` (`client_id`, `client_secret`, `redirect_url`, `HOLTMAIL_OAUTH_*`), access token cached until two minutes before expiry, rotated refresh tokens stored, `invalid_grant` marks `needs_sign_in` and the scheduler and Sync now skip the account, password accounts can move to OAuth through Sign in again, `holtmail account add-oauth`. API: `/api/oauth/start`, `/api/oauth/finish`, `/api/oauth/callback` (303 back to `return_to`), `oauth_provider` and `needs_sign_in` on accounts. Web: Sign in with Google/Microsoft in the connect form with a paste step, Sign in again and Switch to … sign-in on account rows, Sign-in expired status, Thunderbird note. Checked with the real binary and curl: the Google and Microsoft authorize addresses carry the right client, scope, redirect, PKCE and hint; a forged state, a refusal, a reused state and a pasted non-address are refused with readable reasons; the callback with an unknown state redirects with `oauth_error`; nothing about states or codes reaches the log; `account add-oauth` prints the address and refuses a bad paste. Not verified: a real exchange with Google or Microsoft (whether Thunderbird's clients accept `http://127.0.0.1` without a port from another app, Microsoft's current redirect registration, consent screens), XOAUTH2 against real Gmail/Outlook IMAP, own-client registration, device code flow, the web flow in a browser (Phase 12 checks real services).
- Phase 8 done: Gmail labels and flag changes. `holtmail-source`: `RemoteFolder.label`, `RemoteMessage.labels` and `server_id`, `SourceSession::flags(uids, changed_since)` returning `RemoteFlags`; `FakeSource` gained label folders, labelled delivery, flag and label changes and mod-sequences. `source-imap`: capabilities after sign-in, `X-GM-EXT-1` marks Gmail, `ENABLE CONDSTORE` when offered so `EXAMINE` reports `HIGHESTMODSEQ`; Gmail fetches add `X-GM-MSGID X-GM-LABELS` (async-imap parses them; the still-escaped quoted strings are unescaped and decoded from modified UTF-7 in `labels.rs`); every selectable Gmail folder except All Mail, Trash and Spam is a label folder; flag reads with `UID FETCH a:b (UID FLAGS [X-GM-LABELS]) [(CHANGEDSINCE n)]`. Store: `LabelStore` (`set_labels`, `labels`, `list_labels` with counts), `MessageStore::set_flags`, labels in `MessageExtras`, `MessageQuery.label`, `FolderCursor.flags_checked_at`; migration 0006 (`labels`, `message_labels`, the cursor column); conformance cases for flags and labels. Ingest: `Placement.labels` (`None` keeps them), labels in the index document. Index: `IndexDocument.labels`, `Clause::Label` (whole name, any case) with conformance cases; `index-tantivy` has a `labels` field, and an index of the older schema is emptied on open (`was_emptied`), recorded as the setting `index_rebuild`, and `serve` rebuilds it in the background at start. Query: `label:`. Sync: label folders skipped while All Mail syncs (unless named in `include`, or All Mail excluded); flag and label changes since the last mod-sequence, else a full flag read once a day in batches of 1000; changed messages reindexed; `flags_changed` in reports, the CLI and Tasks. API: `GET /api/archives/{id}/labels`, `?label=` on message lists, `labels` on summaries and details; client regenerated with `labelsQuery` and a label argument on `messagesInfiniteQuery`. Web: Labels section under the folders, `/a/$id/label?name=` lists, quiet hairline chips in rows (user labels only, at most two and `+N`, plain text inside the listbox), linked label badges in the reader, `label:` in the operator row, labels in ⌘K. Decisions: existing Gmail accounts keep the location rows of label folders already synced (they stop syncing, counts freeze; nothing deleted), and their next sync archives All Mail, where known messages only gain a location and labels; label changes do update the index document (reindex of that message); `X-GM-MSGID` is carried as `server_id` but not stored yet; imports leave labels alone (Takeout's `X-Gmail-Labels` header is not read yet). Checked with the real binary and curl: an index with the old schema is emptied by `holtmail archives`, `serve` rebuilds it at start and clears the setting, migration 0006 applies, `/labels`, `?label=` and `label:` answer, nothing about mail reaches the log. Not verified: real Gmail (capabilities, `X-GM-LABELS` quoting of real system and non-ASCII labels, `ENABLE CONDSTORE`, `CHANGEDSINCE` on All Mail), non-Gmail servers with `CONDSTORE` and real flag changes, the daily scan on a large mailbox, the web views in a browser, the Docker image with the new crates (all pure Rust; not rebuilt) (Phase 12).
- Phase 9 done: attachments. New `holtmail-extract` (object-safe `Extractor`, `File`, `ExtractError::{TooLarge, Unreadable, TimedOut}`, `MAX_INPUT` 25 MiB, `MAX_TEXT` 1 MiB, conformance suite) and `holtmail-extract-basic` (text/CSV/JSON/XML, HTML via mail-parser's `html_to_text`, PDF via `lopdf` 0.47 without default features and with 64 MiB stream limits, DOCX/XLSX/PPTX/ODT/ODS/ODP via `zip` + `quick-xml` with 10 000 entries and 128 MiB unpacked at most; blocking pool, `catch_unwind`, 30 s timeout; type from the file name for generic types). New `holtmail-thumbnail` (a library, not a contract: `image` with png/jpeg/gif/webp only, format sniffed from bytes, 25 MiB / 40 MP / 20 000 px / 256 MiB alloc limits, EXIF orientation, 320 px, JPEG q80 or PNG with alpha). Store: `DerivedStore` (`derived`, `set_derived`, `clear_derived`), `Derived::{Blob, Nothing}`, `DerivedKind::{Thumbnail, Text}`; migration 0007 `derived`; `delete_archive` also releases derived records and blobs; conformance case. Ingest: `with_extractor`, text per attachment blob cached as a derived blob, unreadable files recorded once, timeouts and unsupported types not recorded, 4 MiB per message. Index: `SearchResults.attachment_hits` (hits that match only through attachment text; tantivy re-runs the page's hits with words on the other fields), `Clause::Attachment` and `attachment:` in the query language, conformance cases. Server: `Services::ingest()` with the extractor everywhere, `holtmail reindex --refresh-attachments`, a background pass at start over messages with attachments until the setting `attachment_text` is set (reusing the Phase 8 path). API: `GET /api/messages/{id}/attachments/{ordinal}/thumbnail` (same access check, nosniff, CSP sandbox, two at a time, failures recorded), `thumbnail_url` on attachments, `attachment_match` on summaries; client regenerated. Web: tiles load `thumbnail_url` lazily and fall back to the icon, only types the server shows inline open in a new tab (`opensInBrowser`), an "In attachment" chip in search rows, `attachment:` in the operator row. Decisions: thumbnails on demand rather than at ingest (most are never viewed; sync stays fast); extraction inline during ingest, bounded by its limits; `.doc`/`.xls`/RTF/`message/rfc822` attachments and OCR not read. Checked with the real binary and curl: an mbox with an invented PNG, DOCX and hand-written PDF imports with two texts recorded; `serve` ran the start-up pass once and set `attachment_text`; `lighthouse` (DOCX) and `ferry` (PDF) find the message with `attachment_match: true`, `harbour` (subject) with `false`, `attachment:rota` finds it and `attachment:harbour` does not; the PNG's thumbnail is a 320×160 JPEG with nosniff and `sandbox`, served again byte for byte from the cached blob; the PDF's thumbnail is `404`, an anonymous request `401`; `reindex --refresh-attachments` dropped and remade both texts; no file name or attachment word in the log. `mise run check` green (180 Rust tests, 122 web tests), knip clean. Not verified: real-world PDFs and office files from other producers (fixtures are generated in tests), scanned PDFs (no OCR), extraction speed on a large archive, the web views in a browser, the Docker image with the new crates (all pure Rust; not rebuilt) (Phase 12).
- Phase 10 done: export, backup and restore. New `holtmail-export` (a library like import: `Exporter::prepare` checks archive, folder and index before anything is written, `Export::write` streams to any `AsyncWrite`, newest first, pages of 500, missing or corrupt blobs skipped and counted; mboxrd with `From <sender> <asctime UTC>` separators, `>`-quoting of `>*From ` lines and an empty line in the message's own line ending; zip of `.eml` through the `zip` crate's stream writer with a shared buffer drained after each entry, deflate on a blocking thread, entries `<folder dirs>/<date> <subject> <16 hex of blob>.eml`, every segment made safe (no `.`/`..`, separators, controls, Windows device names), account name first when the archive has several accounts, one entry per message under its first folder). `holtmail-import` now drops the empty separator line before each `From ` (`\n` or `\r\n`), so mbox exports read back to the same blobs; mbox files imported earlier kept that line, so re-importing such a file stores those messages again (pre-release, accepted). Store: `InventoryStore::referenced_blobs(after, limit)` (messages, attachments, saved images, derived files; ascending, distinct, keyset) in the contract with a conformance case; SQLite merges four index range scans; migration 0008 indexes `messages.blob_id`; `SqliteStore::snapshot` (`VACUUM INTO`, refuses an existing path) and `close`. API: `GET /api/archives/{id}/export?format=mbox|zip&folder|label|q` streamed through a duplex pipe, `attachment` disposition, `no-store`, `400`/`404` before streaming, a failed export ends the body with an error instead of a clean end; members export granted archives (same access rule as reading). Server: `holtmail export` (archive by name or id, folder by path, `account/path` or id, format from the extension, `-` for stdout, `.part` then rename, never overwrites; `--query` needs the index, so not while `serve` runs), `holtmail backup` (snapshot in `<data_dir>/.backup-*`, manifest with counts first, blobs through the `BlobStore`, so S3 blobs too unless `--skip-blobs`; key only with `--include-secret-key`, else a warning; missing blobs named and the command fails after writing), `holtmail restore` (empty target only, manifest first and version check, fixed entry names so nothing in the tar chooses a path, every blob hashed before `put`, then every referenced blob checked with `contains`, `index_rebuild` set and a full reindex unless `--skip-reindex`); `open_blobs` split out of `Services::open`; `reindex` takes the store and an `Ingest`, so restore indexes without opening `secret.key`. New dependency: `tar` 0.4 (no default features). Client regenerated; `exportUrl`, `ExportFormat`, `ExportScope`. Web: a download button in the list header (all mail, folder, label or search) with _mbox file_ and _Zip of .eml files_ as plain `download` links, an _Export_ submenu per archive in Settings → Archives, two ⌘K actions for the whole archive; `lib/export.ts` with tests. Checked with the real binary: an invented mbox imported, exported as mbox, zip, folder zip and `--query` mbox via the CLI; re-imported into two new archives and into itself: 0 new blobs (3 before, 3 after), 0 new messages in the same archive; `curl` export with the session cookie gives the same mbox byte for byte as the CLI, search zip and folder zip, `400` for a bad format and two filters, `401` without a session, the log has counts only; `backup` while `serve` ran (with and without the key, and `--skip-blobs`), `restore` into empty directories (key `0600` and identical, non-empty target refused, a blob with one changed byte stops the restore, a backup without blobs warns about the 3 missing), `serve` on the restored directory finds `ferry` and exports the same mbox byte for byte; with `--skip-reindex` `serve` rebuilt the index at start. Not verified: the web views in a browser, exports and backups of a large archive (memory and speed), zips over 4 GiB (zip64 in stream mode), S3 as the blob store for backup and restore (same contract calls, no S3 in tests), a restore across Holtmail versions, Docker.
- Phase 11 done, with `account edit` and "archive only mail older than N days" from Later. Store: `TaskStore` (`save_task`, `append_task_log` with the first-50/last-450 rule, `recent_tasks`, `interrupt_tasks`, `prune_tasks`), `TaskKind`/`TaskState` (new `Interrupted`)/`TaskLevel`/`TaskLine` moved into the store and re-exported by the API, `SyncStore::set_last_run`/`last_runs`; migration 0009 (`tasks`, `task_log`, `account_runs`); conformance cases. Server: the task registry keeps serving from memory and writes behind once a second through a dirty set (records and batched log lines), flushes on `SIGINT`/`SIGTERM`, prunes to 200; `Runner::open` marks leftover running tasks interrupted, loads the last runs into the account status, makes interrupted syncs due at once, removes uploads of interrupted imports; `resume` restarts interrupted older-mail and failed-image work (the job is stored with the task) before saving new mail's images; `configure` moved to `accounts::configure` and shared with the CLI; `holtmail account edit <id>` (name, host, port, security, user name, `--exclude`/`--clear-exclude`, `--interval`, `--min-age-days`, `--pause`/`--resume`, `--password-stdin`) opens only the database, plus `secret.key` and the OAuth clients when it signs in. Sync: `SyncOptions::min_age`, `FolderReport::deferred`, `FakeSource::deliver_received`; the task log says how many messages wait. API: `min_age_days` on accounts and in `PATCH` (1-3650, 0/null clears), `interrupted` task state. Web: _What to archive_ submenu per source, _Interrupted_ badge and progress in the warning color with an explanation, notes updated. Checked with the real binary: a sync hanging on a silent port, `kill -9`, restart: the task shows `interrupted` with its log and a new sync starts at once; a failed sync's error and time survive a restart and the next sync is 15 minutes after it; `account edit` while `serve` ran set interval, minimum age, pause and name, refused an unreachable server and a 5000-day age without storing anything, and the API showed the changes; logs carry no addresses. Not verified: a successful connection change from the CLI (needs a real or fake IMAP server outside tests; the path is the one the API uses), resuming remote image work after a real interruption (needs internet images), the web views in a browser.
- JMAP source done (Later). Contract: `FolderStatus::uids_ascend` (IMAP and `FakeSource` `true`; `FakeSource::mark_unordered`, `deliver_at`); `holtmail-sync` fetches every on-server UID without a live location (minus UIDs that failed `max_attempts` times) when it is `false`, and judges minimum age on every non-retry message there. New `holtmail-source-jmap` (reqwest + url + serde_json): session from the given address or `/.well-known/jmap` (redirects followed, never off https), Basic or bearer, mail capability and primary account, `Mailbox/get` → folders with roles and `/` paths, `Email/query` by `receivedAt` paged with `Email/get` by back-reference, `Email/get` batched to `maxObjectsInGet` (≤500) per call and `maxCallsInRequest` (≤16) per request, raw blobs via `downloadUrl` four at a time, keywords → flags (`$seen` → `\Seen` etc.), `404`/not found left out; tests against an in-process axum fake. Server: `AccountKind::Jmap` composed in `source_for`, `account add-jmap --archive --url [--username] [--token-stdin] [--exclude]` (signs in first), `account edit --url --auth` for JMAP accounts, `configure` checks a new JMAP connection or secret by signing in and asks for a new secret when the sign-in kind changes. API: `POST /api/archives/{id}/accounts/jmap` (managers only), `jmap` connection on accounts (hidden from members), `PATCH` with `jmap`; client regenerated with `addJmapAccount`. Web: _Connect over JMAP instead_ under the IMAP form, a JMAP form with a Fastmail preset (API token to `https://api.fastmail.com/jmap/session`) or any server (address, password or API token), Edit connection and Change API token for JMAP accounts, a JMAP tag on the row. Checked with the real binary against a local Python fake JMAP server on 127.0.0.1: `add-jmap` refuses `http://` to a public host and a wrong token, adds token and password accounts, `sync` archives three mailboxes with an email in two of them stored once, a message moved in with an older `receivedAt` is found on the next sync, `account edit --auth password` without a secret is refused and with one stored; via curl: a refused token is `400` with a readable reason, a token account is created with `jmap` in the DTO and synced, `PATCH` with `jmap` normalises the address and keeps the token, an IMAP `connection` on it is `400`, search finds its mail with `\Flagged` from `$flagged`; no token, password, address or subject in the log. `mise run check` green (212 Rust tests, 130 web tests). Not verified: any real JMAP server (Fastmail, Stalwart, Cyrus: session shapes, limits, `Email/query` limits and `total`, download content types, rate limits), the Fastmail token hint's settings path, large mailboxes (every sync lists all ids; `Email/changes` not used), the web forms in a browser. `knip` reports `minAgeChoices` in `apps/web/src/lib/schedule.ts` unused; it predates this step.
- Microsoft Graph source done (Later). Contract: `SourceSession::state()` and `unchanged_since(state)` with defaults (`None`), so IMAP and JMAP are unchanged; `FakeSource::track_changes`/`listings`. Sync: a folder whose stored `FolderCursor::state` the source calls unchanged is skipped (`FolderReport::unchanged`); the state is dropped before any other sync and stored again only when nothing was deferred and no failure is left to retry; no migration (the `state` column existed). OAuth: `holtmail_oauth::Access::{Imap, Graph}`, `Provider::scope(access)`, `Client::for_access`; Graph asks for `https://graph.microsoft.com/Mail.Read https://graph.microsoft.com/User.Read offline_access` (Thunderbird's client declares `Mail.ReadWrite`, `MailboxFolder.ReadWrite`, `Mail.Send`, `User.Read` behind `mail.graph.enabled`; Holtmail asks for less). New `holtmail-source-graph` (reqwest + url + serde_json + tokio time): folders with hidden and child folders, search folders skipped, well-known roles, delta listing paged to the delta link, `$value` downloads four at a time, `isRead`/`flag`/`isDraft` flags, immutable ids, `Retry-After` throttling (five tries, at most two minutes), the token only ever sent to the configured Graph origin, `/me` for the address; tests against an in-process axum fake. Server: `AccountKind::Graph` in `source_for`, `Pending.access`, refreshes per account kind, `finish_oauth` checks a Graph sign-in by listing folders, records the address `/me` names and refuses signing in again as another mailbox, `configure` refuses server settings and passwords for Graph accounts, `holtmail account add-graph --archive --email [--name]`. API: `kind` (`imap`, `graph`) on `POST /api/oauth/start` (`graph` needs `microsoft`; ignored for `account_id`, whose kind decides), `OAuthStart.kind`; client regenerated, `OAuthKind`. Web: _Connect Microsoft 365 or Outlook.com through Microsoft Graph instead_ under the IMAP form for any address, a Graph form (address, name, Thunderbird note, Sign in with Microsoft, same paste step), a Graph tag on the row; `minAgeChoices` no longer exported (only `minAgeOptions` uses it), so knip is clean. Checked with the real binary: `account add-graph` prints a Microsoft authorize address with Thunderbird's client, the Graph scopes, `http://127.0.0.1`, the hint and S256, and refuses a paste without state; via curl `/api/oauth/start` with `kind: graph` gives the Graph scopes, `google` + `graph` and an unknown kind are `400`, without `kind` the IMAP scope stays; a refusal at the sign-in page is a readable `400`; no address or state in the log. Not verified against Microsoft: whether Thunderbird's client is granted `Mail.Read`/`User.Read` (incremental consent), a real code exchange, Graph's real paging sizes for delta, delta link expiry, `includeHiddenFolders` and well-known names on real tenants and Outlook.com, `$value` of large messages, throttling behaviour, the web form in a browser, a Graph sync end to end with the binary (needs a real token).
- Review fix: editing a connection no longer sends a stored secret to another server. `accounts::configure` (API `PATCH` and `account edit`) refuses, before any sign-in, an IMAP change to another host or effective port or from TLS/STARTTLS to `plain`, or a JMAP address on another origin, without a new password or token (`400` "Enter the password again when changing the server."; the CLI says to pass `--password-stdin`); a security change on the same host and port that keeps encryption keeps the password (decision: same server, same secret). OAuth IMAP accounts refuse any host, port or security change, so their access token only goes to the provider's server; Graph accounts already refused server settings. Web: `src/lib/connection.ts` (`imapMoves`, `jmapMoves`, `effectivePort`) makes the password or token field required with a note in Edit connection; OAuth accounts get read-only server fields. Tests: server unit tests on the real rule (nothing stored), the API's fake models it for the `400`, web helper tests. Not verified: the dialog in a browser.
