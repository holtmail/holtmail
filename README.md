# Holtmail

A self-hosted email archiver. Holtmail connects to your mail accounts over IMAP, keeps every message exactly as it was received, and gives you fast full-text search and a quiet web interface to read your archive.

No open core, no enterprise edition: every feature, SSO included, is in this repository under one license.

Status: early. Archiving from IMAP and from mbox, Maildir, `.eml` and zip files works, with search, threads, a safe reader, and people with per-archive access, all managed from the web app. See the plan in [docs/PLAN.md](docs/PLAN.md).

## Try it

```bash
mise install
cargo build --release
bun install && bun run build

./target/release/holtmail import --archive Personal ~/Downloads/mail.mbox
HOLTMAIL_WEB_DIR=apps/web/dist ./target/release/holtmail serve
```

On the first start the server prints a one-time setup link; open it to create the owner account. Holtmail then walks you through connecting a mailbox (Gmail, Outlook, iCloud, Fastmail, Yandex or any IMAP server) or uploading mbox, `.eml` and zip exports. Everything else, from more archives to the people who may read them, lives in Settings. The same can be done from the command line:

```bash
./target/release/holtmail account add-imap --archive Personal --host imap.example.com --username ada@example.com
./target/release/holtmail sync
```

`serve` keeps syncing every 15 minutes. Everything lives in `./data`: the metadata database, message bytes (or S3), the search index and the key that seals account passwords. See [apps/server/docs](apps/server/docs/README.md) for commands and configuration.

## How it is built

Rust on the server, React in the browser, one monorepo of small modules behind contracts: storage, search, mail sources and secrets can each be replaced. [docs/README.md](docs/README.md) links every module's documentation; [AGENTS.md](AGENTS.md) has the rules for contributing.

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
