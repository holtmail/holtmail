# Holtmail docs

- [Plan](PLAN.md) — what Holtmail is, its principles and modules
- [Interface design](design.md) — how the web app should look and feel
- [Work plan](WORKPLAN.md) — the current checklist, decisions and lessons from Mail-Archiver

## Modules

| Module | Docs |
| --- | --- |
| `holtmail-core` | [Ids, timestamps, addresses](../crates/core/docs/README.md) |
| `holtmail-blob` | [Contract for raw message storage](../crates/blob/docs/README.md) |
| `holtmail-blob-fs` | [Blob storage on a local disk](../crates/blob-fs/docs/README.md) |
| `holtmail-blob-s3` | [Blob storage on S3-compatible services](../crates/blob-s3/docs/README.md) |
| `holtmail-mime` | [Parsing raw messages](../crates/mime/docs/README.md) |
| `holtmail-store` | [Contract for metadata](../crates/store/docs/README.md) |
| `holtmail-store-sqlite` | [Metadata on SQLite](../crates/store-sqlite/docs/README.md) |
| `holtmail-sanitize` | [Safe message HTML](../crates/sanitize/docs/README.md) |
| `holtmail-fetch` | [Contract for fetching remote images](../crates/fetch/docs/README.md) |
| `holtmail-fetch-http` | [Fetching over HTTP, public addresses only](../crates/fetch-http/docs/README.md) |
| `holtmail-remote` | [Saving remote images into the archive](../crates/remote/docs/README.md) |
| `holtmail-index` | [Contract for search](../crates/index/docs/README.md) |
| `holtmail-index-tantivy` | [Search on Tantivy](../crates/index-tantivy/docs/README.md) |
| `holtmail-query` | [Search language](../crates/query/docs/README.md) |
| `holtmail-ingest` | [From raw bytes to an archived message](../crates/ingest/docs/README.md) |
| `holtmail-import` | [Reading mbox, Maildir, .eml and zip](../crates/import/docs/README.md) |
| `holtmail-api` | [HTTP API](../crates/api/docs/README.md) |
| `holtmail` (apps/server) | [The binary, commands and config](../apps/server/docs/README.md) |
| `holtmail-secret` | [Contract for sealing credentials](../crates/secret/docs/README.md) |
| `holtmail-secret-local` | [Sealing with a local key file](../crates/secret-local/docs/README.md) |
| `holtmail-source` | [Contract for mail servers](../crates/source/docs/README.md) |
| `holtmail-source-imap` | [IMAP](../crates/source-imap/docs/README.md) |
| `holtmail-sync` | [Incremental archiving](../crates/sync/docs/README.md) |
| `holtmail-auth` | [Sign-in, sessions and the setup link](../crates/auth/docs/README.md) |
| `@holtmail/tokens` | [Design tokens](../packages/tokens/docs/README.md) |
| `@holtmail/ui` | [Components](../packages/ui/docs/README.md) |
| `@holtmail/api-client` | [Typed API client](../packages/api-client/docs/README.md) |
| `apps/web` | [The web app](../apps/web/docs/README.md) |
