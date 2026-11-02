# Holtmail — plan

Holtmail is a self-hosted email archiver. It connects to mail accounts over IMAP, JMAP and Microsoft Graph, keeps a copy of every message for as long as you want, and lets you search and read the archive in a fast web interface. It is for a person who wants their mail out of a provider's hands as well as for a company that must keep mail for years.

Status: design. The first modules are [holtmail-blob](../crates/blob/docs/README.md) and [holtmail-blob-fs](../crates/blob-fs/docs/README.md).

## Principles

- **Every feature is free.** There is no enterprise edition. SSO, roles, retention, legal hold and audit live in this repository under the same permissive license as everything else.
- **Raw bytes are the truth.** Messages are stored byte for byte, addressed by their BLAKE3 hash. Everything else is derived and can be rebuilt.
- **Bytes on disk or S3, never in the database.** Message bodies and attachments live in a `BlobStore` (local disk or S3-compatible storage). The database holds metadata only, so it stays small, fast to back up and cheap to migrate.
- **Everything behind contracts.** Mail sources, storage, the metadata store, the search index and authentication are contracts with replaceable implementations. A new provider is a new crate; nothing else changes.
- **Read-only towards the mail server.** Archiving never changes, moves or deletes mail on the source account unless a user explicitly turns that on for an account.
- **Admins run the instance, they do not read the mail.** Managing Holtmail and reading an archive are separate permissions. See [Users and access](#users-and-access).
- **One binary.** `holtmail serve` runs everything with SQLite and files on disk. Postgres and S3 are options, not requirements.
- **Light and beautiful.** A quiet interface with square corners, hairline borders and meaningful motion. See [design.md](design.md).

## What it does

**Archive.** Connect an account, pick folders, and Holtmail keeps them in sync on a schedule: new mail arrives within minutes, nothing is fetched twice, and a message seen in several folders or accounts is stored once. Accounts: IMAP (password or OAuth2), Gmail (labels kept), Microsoft 365 and Outlook.com through Graph, JMAP (Fastmail, Stalwart), and import-only archives.

**Find.** Full-text search over subjects, bodies, addresses and attachment text, with a query language (`from:ada@example.com has:attachment before:2024-01-01 "quarterly report"`), facets by account, folder, sender and year, and saved searches.

**Read.** Threads, safe HTML rendering with remote content blocked, attachments with previews, the raw source with headers and DKIM/SPF results for forensics.

**Keep and let go.** Retention policies per account, folder or tag; legal hold that overrides them; an optional deletion lock; deleting from the mail server after a period, off by default.

**Move.** Import mbox, Maildir, `.eml` and Google Takeout; export the same formats; restore single messages or whole folders back to any connected mailbox.

**Integrate.** A read-only REST API with scoped tokens, an MCP server so assistants can search an archive with a user's permissions, and Prometheus metrics.

## Users and access

The model stays small and explicit:

- **Users** sign in with a passkey, a password, or OIDC. There is no shared admin password in an environment variable: on first start the server logs a one-time setup link, and the first person to open it becomes the owner.
- **Instance roles** are about running Holtmail, never about reading mail: `owner` (everything, including transferring ownership), `admin` (users, settings, storage, sync health), `member` (their own archives only).
- **Archives** are what you read. An archive is one or more connected accounts. Its creator is its owner, and access is granted per archive to users or groups: `reader` (search and read), `manager` (also change sync, retention, export and delete).
- **Groups** are local or come from OIDC claims, so a company manages access in its identity provider.
- **Auditor** is a grant on an archive for compliance review. Every search and every opened message is written to the audit log, and owners can require a reason before access.
- **Audit log** records sign-ins, grants, searches, opened messages, exports and deletions. Nobody can edit it, admins included.
- **API tokens** belong to a user, carry a subset of that user's grants and expire.

In a single-user install all of this collapses into one owner who also owns every archive, and the interface hides what does not apply.

## Modules

Contracts and their implementations:

| Contract          | What it covers                                                                     | Implementations                                   |
| ----------------- | ---------------------------------------------------------------------------------- | ------------------------------------------------- |
| `holtmail-blob`   | Raw message bytes and extracted attachments, content-addressed                     | `blob-fs`, `blob-s3`                              |
| `holtmail-store`  | Metadata: accounts, folders, messages, threads, sync state, users, grants, audit   | `store-sqlite`, `store-postgres`                  |
| `holtmail-source` | Listing folders and fetching new messages from a mail account, incrementally       | `source-imap`, `source-graph`, `source-jmap`      |
| `holtmail-index`  | Full-text search over messages                                                     | `index-tantivy`                                   |
| `holtmail-auth`   | Signing users in                                                                   | `auth-passkey`, `auth-password`, `auth-oidc`      |
| `holtmail-secret` | Encrypting stored account credentials and OAuth tokens                             | `secret-local`                                    |
| `holtmail-import` | Reading archives from files                                                        | `import-mbox`, `import-maildir`, `import-eml`     |

Modules that are not behind a contract, because there is one right way to do them:

| Module                 | Job                                                                                        |
| ---------------------- | ------------------------------------------------------------------------------------------ |
| `holtmail-core`        | Ids and types shared by several contracts                                                  |
| `holtmail-mime`        | Parsing raw bytes into headers, bodies and attachments (on top of `mail-parser`)           |
| `holtmail-sanitize`    | Making message HTML safe to show (on top of `ammonia`)                                     |
| `holtmail-query`       | Parsing the search language into a structured query for `holtmail-index`                   |
| `holtmail-sync`        | The sync engine: source → blob → store → index, with retries and resumable state         |
| `holtmail-jobs`        | Scheduling and running sync, retention and export jobs on top of the store                 |
| `holtmail-access`      | Deciding who may do what: roles, grants, groups                                            |
| `holtmail-api`         | The HTTP API (`axum`) and its OpenAPI description                                          |
| `holtmail-mcp`         | The MCP server over the same access rules as the API                                       |
| `apps/server`          | The `holtmail` binary: config, wiring implementations, `serve`, `sync`, `import`, `export` |
| `apps/web`             | The React app                                                                              |
| `@holtmail/tokens`     | Design tokens: color, type, spacing, motion                                                |
| `@holtmail/ui`         | shadcn-based components built only on tokens                                               |
| `@holtmail/api-client` | A typed client generated from the server's OpenAPI description                             |

## Storage

- **Messages.** The raw RFC 5322 bytes of every message are one blob.
- **Attachments.** Extracted on ingest and stored as their own blobs, so the same file mailed to ten people or forwarded ten times is stored once. The message blob stays complete and unchanged.
- **Compression and encryption** are wrapping stores over any `BlobStore` (`blob-zstd`, `blob-encrypted`), so they work the same on disk and on S3.
- **S3** works with AWS, MinIO, Garage, Versity and other S3-compatible services. Object Lock can back legal hold and the deletion lock.
- **Deletion** of a blob happens only when no message or attachment references it, in a background sweep.

## Sync

- IMAP: per folder, `UIDVALIDITY` plus the highest seen `UID`; `CONDSTORE`/`QRESYNC` when the server has them. A changed `UIDVALIDITY` re-lists the folder, and content addressing makes that cheap: known messages are recognised by hash and not stored twice.
- Gmail: archive from `[Gmail]/All Mail` and keep labels from `X-GM-LABELS` instead of fetching every label folder.
- Microsoft: Graph delta queries per folder; OAuth2 device code flow for personal accounts, an app registration for tenants with bulk import of their mailboxes.
- JMAP: `Email/changes` with the saved state string.
- OAuth2 (`XOAUTH2`) for Gmail and Microsoft 365 from the first release that supports them.
- A sync never deletes from the archive because a message disappeared from the server; it records when it was last seen.

## Web app

It looks and feels like a beautiful mail client, not an admin panel for an archive. Opening Holtmail lands you in your mail; storage, sync and users live in settings, out of the way. Anyone who has used Apple Mail or Gmail knows how to use it without reading anything.

- **Three panes**: archives and folders, the message list, the reader. Each pane collapses on small screens.
- **A list made for reading**: sender, subject and a one-line preview, grouped by day ("Today", "Last week", "March 2019"), with flags, attachment and thread indicators from the original mailbox.
- **Conversations**: a thread reads top to bottom as one page, older messages collapsed to one line each, quoted text folded.
- **Read-only, honestly**: no compose button pretending to be a mail client. Reply opens the user's own mail client (`mailto:` with the right headers); the actions are those of an archive: export, restore to a mailbox, tag, hold, show the source.
- **Command palette** (`⌘K`) for every action and for jumping to an archive, folder or saved search.
- **Search first**: the search field is always one key away (`/`), with suggestions for operators, senders and folders.
- **Timeline**: a year-by-month histogram of an archive, which also works as a date filter.
- **Attachments view**: every attachment of an archive as a grid, filterable by type.
- **Sync status**: what is syncing now, what failed and why, in plain language.
- **Keyboard**: `j`/`k`, `o`, `u`, `/`, `g a`, `?` for the full list.
- **Languages**: English and Russian first, through ICU message files.

## Milestones

Each milestone ends in something usable.

1. **Foundation.** Workspace, CI, `mise` tasks; `holtmail-core`, `holtmail-blob` with `blob-fs` and `blob-s3`; `holtmail-store` with `store-sqlite`; `holtmail-mime`. Web tooling: `apps/web`, `@holtmail/tokens`, `@holtmail/ui` with the first shadcn components restyled to Carbon.
2. **First archive.** `source-imap` with a password, `holtmail-sync`, `index-tantivy`, `holtmail-query`, `holtmail-api`, and a web app that lists, reads and searches one archive for one owner. `holtmail import` for mbox and `.eml`. Docker image.
3. **Real providers.** Gmail and Microsoft OAuth2, `source-graph`, `source-jmap`, labels, attachments as separate blobs, `blob-zstd`, the timeline and attachments views.
4. **People.** Passkeys and OIDC, roles, archives with grants, groups, API tokens, the audit log, `store-postgres`.
5. **Compliance.** Retention, legal hold, deletion lock, S3 Object Lock, export, restore to a mailbox, `blob-encrypted`.
6. **Integrations.** MCP server, Prometheus metrics, Helm chart, Google Takeout import.

## Stack

| Area        | Choice                                                    |
| ----------- | --------------------------------------------------------- |
| Server      | Rust 2024, Tokio, axum                                    |
| MIME        | `mail-parser`                                             |
| IMAP        | `imap-codec` / `imap-next`                                |
| JMAP        | `jmap-client`                                             |
| Search      | `tantivy`                                                 |
| Database    | SQLite and Postgres through `sqlx`                        |
| S3          | `aws-sdk-s3` or `object_store`, decided in milestone 1    |
| Web         | React, Vite, TypeScript, TanStack Router and Query        |
| UI          | shadcn/ui on Radix, Tailwind CSS v4, Motion, IBM Plex — see [design.md](design.md) |
| Tools       | mise, cargo fmt, clippy                                   |

## Prior art

[Mail-Archiver](https://github.com/s1t5/mail-archiver) (.NET, Postgres) covers much of the same ground: IMAP and Graph accounts, retention, mbox/EML import and export, restore, OIDC, an access log and an MCP server. Its feature list is a useful checklist. Holtmail differs where it matters most to us: the interface, a user model where running the instance and reading mail are separate, message and attachment bytes kept outside the database with deduplication, and replaceable modules.

## License

`MIT OR Apache-2.0`, the usual dual license of the Rust ecosystem, so companies can run, embed and extend Holtmail without legal review hurdles. Apache-2.0 adds an explicit patent grant; MIT keeps compatibility with GPLv2 projects. Contributions come in under the same terms (inbound = outbound), with no CLA.
