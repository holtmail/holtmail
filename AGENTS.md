# Agent guide

Instructions for AI agents and humans working in this repository. Read [docs/PLAN.md](docs/PLAN.md) for the product picture and [docs/design.md](docs/design.md) before touching the interface.

Holtmail is a self-hosted email archiver: it pulls mail from IMAP and JMAP accounts, stores every message once, indexes it for full-text search and serves it through an HTTP API and a web interface. Rust on the server, React in the browser, one monorepo.

## Commands

- `cargo fmt --all` — format Rust
- `cargo clippy --workspace --all-targets -- -D warnings` — lint
- `cargo test --workspace` — tests; `cargo test -p holtmail-blob-fs` for one crate
- `mise run check` — everything CI runs, Rust and web

Run `mise run check` before considering a change done. Tools are pinned in `mise.toml` and `rust-toolchain.toml`; run them through `mise exec --` if they are not on your PATH.

## Layout

- `crates/*` — Rust libraries: contracts and their implementations
- `apps/server` — the `holtmail` binary, the only place that picks implementations
- `apps/web` — the React app
- `packages/*` — TypeScript packages for the web app (`ui`, `tokens`, `api-client`)
- `docs/` — project-wide docs; every crate, app and package also has its own `docs/` folder

Crate directories drop the prefix, package names keep it: `crates/blob-fs` is `holtmail-blob-fs`. TypeScript packages are `@holtmail/<name>`.

## Modules and contracts

Holtmail is many small modules, and any of them can be swapped for another one that keeps the same contract. Mail sources, blob storage, the metadata store, the search index and authentication are all behind contracts.

- **Contract crates** (`holtmail-blob`, `holtmail-store`, `holtmail-source`, `holtmail-index`, `holtmail-secret`, ...) hold only traits, the types in their signatures and their error type. No I/O, no provider code.
- **Implementation crates** are named `<contract>-<kind>` (`blob-fs`, `blob-s3`, `source-imap`, `index-tantivy`). They depend on their contract and on `holtmail-core`, never on another implementation.
- **One composition root.** Only `apps/server` depends on implementation crates; it builds them from config and hands them out as `Arc<dyn Trait>`. Library code receives contracts, never constructs implementations.
- **Object-safe traits.** Every contract trait is `Send + Sync` and usable as `dyn Trait`; async methods use `#[async_trait]`. Generic parameters on contract methods are not allowed.
- **Nothing leaks.** Contract signatures use the contract's own types. No `async_imap`, `tantivy`, `sqlx` or `aws_sdk` types cross a contract boundary, including in errors.
- **Errors per contract.** Each contract defines one `thiserror` enum; implementations map their failures into it. Libraries never return `anyhow::Error`; only `apps/server` may use `anyhow`.
- **Conformance tests.** Each contract ships a test suite behind its `testing` feature (`holtmail_blob::testing::conformance`). Every implementation runs it in its own tests, so a new implementation proves it keeps the contract by passing the same suite.
- **Guarantees, not mechanics.** Every public trait, method and type gets a doc comment that says what it guarantees: ordering, idempotency, what happens when something is missing, concurrency. It says nothing about how it is done.
- **Small modules.** A crate has one job. When a crate starts doing two things, split it before adding the third.

The web app follows the same idea: it talks to the server only through `@holtmail/api-client`, and components come from `@holtmail/ui`, which takes every color, size and spacing from `@holtmail/tokens`.

## Docs

Every crate, app and package has `docs/README.md`:

1. What the module does, in one paragraph.
2. Its public API: traits, types and functions, with the guarantees from their doc comments.
3. For a contract: what an implementation must do, and how to run the conformance suite.
4. For an implementation: its config, its limits and the provider quirks it deals with.

Longer topics go in more files in the same `docs/` folder. A change to a module's behavior or public API updates its `docs/` in the same change. Link every new module from [docs/README.md](docs/README.md). Anything a change could not prove with tests (real services, real data, the interface by eye) goes into [docs/VERIFY.md](docs/VERIFY.md).

## Rules

- **English** in every file: code, comments, docs, commit messages.
- **No real mail.** Tests, fixtures and docs use invented messages, addresses on `example.com`/`example.org` and made-up names. Never commit a real message, header or address, not even a redacted one; rebuild the shape with invented values. Fixtures are hand-written `.eml` files in the crate's `tests/fixtures/`.
- **Never log mail or secrets.** Logs carry ids, counts and timings, never message content, subjects, addresses, passwords or tokens.
- **Raw bytes are the truth.** The original RFC 5322 bytes of every message are stored unchanged and addressed by their BLAKE3 hash. Parsed fields, indexes and thumbnails are derived data that can be rebuilt from them.
- **Comments are rare.** Doc comments on the public API are required; inside function bodies, comment only what a reader cannot infer (a server quirk, an RFC corner, a deliberate trade-off).
- **Tests sit next to the code** in `#[cfg(test)]` modules; integration tests go in the crate's `tests/`. Tests never touch the network; IMAP and JMAP tests run against in-process fakes.
- **Dependencies** go in `[workspace.dependencies]` in the root `Cargo.toml`, and crates use `dep.workspace = true`.
- **Interface:** square corners and hairline borders, see [docs/design.md](docs/design.md). No `border-radius` other than the `0` from tokens.
- **Commits and PR titles:** one line in Conventional Commits form with the module as scope (`feat(source-imap): ...`, `fix(web): ...`), no body.

## Adding a module

1. Create `crates/<name>/` with `Cargo.toml` naming the package `holtmail-<name>` and inheriting `version`, `edition`, `license` and `rust-version` from the workspace.
2. Add `src/lib.rs`, tests and `docs/README.md`.
3. For an implementation, depend on the contract with `features = ["testing"]` under `[dev-dependencies]` and run its conformance suite.
4. Link the module from [docs/README.md](docs/README.md).
