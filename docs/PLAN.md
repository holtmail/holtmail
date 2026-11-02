# Holtmail — plan

Holtmail is a self-hosted email archiver. It connects to mail accounts over IMAP and JMAP, keeps a copy of every message for as long as you want, and lets you search and read the archive in a fast web interface. It is for a person who wants their mail out of a provider's hands as well as for a company that must keep mail for years.

Status: design. The first modules are [holtmail-blob](../crates/blob/docs/README.md) and [holtmail-blob-fs](../crates/blob-fs/docs/README.md).

## Principles

- **Every feature is free.** There is no enterprise edition. SSO, roles, retention, legal hold and audit live in this repository under the same permissive license as everything else.
- **Raw bytes are the truth.** Messages are stored byte for byte, addressed by their BLAKE3 hash. Everything else is derived and can be rebuilt.
- **Everything behind contracts.** Mail sources, storage, the metadata store, the search index and authentication are contracts with replaceable implementations. A new provider is a new crate; nothing else changes.
- **Read-only towards the mail server.** Archiving never changes, moves or deletes mail on the source account unless a user explicitly turns that on for an account.
- **One binary.** `holtmail serve` runs everything with SQLite and files on disk. Postgres and S3 are options, not requirements.
- **Light and beautiful.** A quiet interface with square corners, hairline borders and fast navigation. See [design.md](design.md).

## Modules

Contracts and their first implementations. Planned crates are not created yet.

| Contract          | What it covers                                                                     | Implementations                    |
| ----------------- | ---------------------------------------------------------------------------------- | ---------------------------------- |
| `holtmail-blob`   | Raw message bytes, content-addressed                                               | `blob-fs`, planned `blob-s3`       |
| `holtmail-store`  | Metadata: accounts, folders, messages, threads, sync state, users                  | `store-sqlite`, `store-postgres`   |
| `holtmail-source` | Listing folders and fetching new messages from a mail account, incrementally       | `source-imap`, `source-jmap`       |
| `holtmail-index`  | Full-text search over messages                                                     | `index-tantivy`                    |
| `holtmail-auth`   | Signing users in                                                                   | `auth-password`, `auth-oidc`       |
| `holtmail-secret` | Encrypting stored account credentials                                              | `secret-local`                     |

Modules that are not behind a contract, because there is one right way to do them:

| Module               | Job                                                                                         |
| -------------------- | ------------------------------------------------------------------------------------------- |
| `holtmail-core`      | Ids and types shared by several contracts                                                   |
| `holtmail-mime`      | Parsing raw bytes into headers, bodies and attachments (on top of `mail-parser`)            |
| `holtmail-sanitize`  | Making message HTML safe to show (on top of `ammonia`)                                      |
| `holtmail-sync`      | The sync engine: source → blob → store → index, with retries and resumable state          |
| `holtmail-api`       | The HTTP API (`axum`) and its OpenAPI description                                           |
| `apps/server`        | The `holtmail` binary: config, wiring implementations, `serve`, `sync`, `import`, `export`  |
| `apps/web`           | The React app                                                                               |
| `@holtmail/tokens`   | Design tokens: colors, type, spacing                                                        |
| `@holtmail/ui`       | Components built only on tokens                                                             |
| `@holtmail/api-client` | A typed client generated from the server's OpenAPI description                           |

## Sync

- IMAP: per folder, `UIDVALIDITY` plus the highest seen `UID`; `CONDSTORE`/`QRESYNC` when the server has them. A changed `UIDVALIDITY` re-lists the folder, and content addressing makes that cheap: known messages are recognised by hash and not stored twice.
- Gmail: archive from `[Gmail]/All Mail` and keep labels from `X-GM-LABELS` instead of fetching every label folder.
- JMAP: `Email/changes` with the saved state string.
- OAuth2 (`XOAUTH2`) for Gmail and Microsoft 365 from the first release.

## Later

Import and export (mbox, Maildir, `.eml`), retention policies and legal hold, deduplicated attachments, an audit log, S3 storage, multi-user with roles and OIDC.

## Stack

| Area        | Choice                                    |
| ----------- | ----------------------------------------- |
| Server      | Rust 2024, Tokio, axum                    |
| MIME        | `mail-parser`                             |
| IMAP        | `imap-codec` / `imap-next`                |
| JMAP        | `jmap-client`                             |
| Search      | `tantivy`                                 |
| Database    | SQLite and Postgres through `sqlx`        |
| Web         | React, Vite, TypeScript, TanStack Router and Query |
| Tools       | mise, cargo fmt, clippy                   |

## License

`MIT OR Apache-2.0`, the usual dual license of the Rust ecosystem, so companies can run, embed and extend Holtmail without legal review hurdles. Apache-2.0 adds an explicit patent grant; MIT keeps compatibility with GPLv2 projects. Contributions come in under the same terms (inbound = outbound), with no CLA.
