# holtmail-category-domains

A `Categorizer` (id `domains`) that looks up the sender's domain in category tables: the bundled CC0 Wikidata table compiled into the binary and any tables handed to it later through `reload`. It is the domain layer of [docs/INSIGHTS.md §4.2](../../../docs/INSIGHTS.md): it votes topics only, never a kind. It also checks downloaded data releases: the signed manifest against compiled-in ed25519 keys, then each file against the manifest. It depends only on `holtmail-category`, the plain format library `holtmail-category-data`, `ed25519-dalek` and `sha2`.

## API

```rust
pub const ID: &str = "domains";
pub const VERSION: &str = "2";                     // lookup rule version
pub const BUNDLED_MAX_BYTES: usize = 2 * 1024 * 1024;

pub fn bundled() -> Result<DomainTable, CategoryError>;

pub struct DomainsCategorizer { .. }               // impl Categorizer, Send + Sync
impl DomainsCategorizer {
    pub fn new(tables: Vec<DomainTable>) -> Result<Self, CategoryError>;
    pub fn with_bundled() -> Result<Self, CategoryError>;
    pub fn reload(&self, tables: Vec<DomainTable>) -> Result<(), CategoryError>;
    pub fn tables(&self) -> Vec<TableInfo>;
}

pub struct TableInfo { pub name: String, pub version: String, pub rows: usize, pub hash: String }
```

| Item | Guarantees |
|---|---|
| `bundled()` | Parses the compiled-in `data/wikidata-categories.tsv.gz`. Deterministic, no I/O. A file that does not parse is `CategoryError::Data` naming the broken rule (`gzip`, `too large`, `order`, ...), never row text. |
| `new(tables)` | A categorizer over `tables` in any order. Two tables with the same header name are refused as `CategoryError::Data("duplicate table name")`. An empty set is allowed and never votes. |
| `with_bundled()` | `new(vec![bundled()?])`. |
| `reload(tables)` | Replaces the whole set atomically. Snapshots taken afterwards see only the new set; snapshots taken before keep the old one. On error the old set stays. Only `apps/server` calls it. |
| `tables()` | Name, version, kept rows and BLAKE3 hash of each loaded table, sorted by name. No rows. For the Settings → About page. |
| `snapshot()` | Clones the current set; never blocks on a reload for longer than a pointer swap. A poisoned lock is recovered. |

**Snapshot version** is `domains-{VERSION}` followed by `:{name}@{version}#{hash}` for each table in name order, or `domains-2:none` with no tables. The hash is over the decompressed file, so the version changes whenever any table's content changes, and `derive_fp` with it (§6.5).

**`categorize`**:

1. No vote when the message is the archive's own mail (`own`) or an inline forward (`forwarded`): the sender is then a person, and the domain says nothing about the mail.
2. The host is the part of `sender_address` after its last `@` (none without an `@`); the registrable domain is `sender_domain`, computed by the runner.
3. Each table answers `DomainTable::lookup(host, registrable)`: the exact host first, then the registrable domain, both compared ASCII-case-insensitively.
4. The answers of all tables are united and reduced to `known()` topics.
5. Empty gives `None`; otherwise `CategoryVote { kind: None, kind_source: Domain, topics, source: Domain }`.

It never panics, whatever the sender holds (none, empty, 10 KiB, non-ASCII, `@` only). It does **not** apply the trust gate: a table can say `finance` for a bank's domain, and the runner keeps that topic only when the message's trust is aligned, so a spoofed sender never gets a sensitive topic.

## Config

None. `apps/server` builds it with `with_bundled()` when `domains` is among the category sources, holds the concrete `Arc<DomainsCategorizer>` and hands it to the runner as `dyn Categorizer`. Downloaded tables (Phase 42) are checked by the server with `verify_manifest` and `check_file` (or `verify_stored` and `check_file` for files already on disk at start) and come in through `reload` together with the bundled one. `UpdateError` is this crate's own type, not a contract type; the server maps it.

## Data releases

A release is `manifest.json`, `manifest.json.sig` (64 raw bytes) and the files the manifest lists; the manifest's shape and rules are in [`holtmail-category-data`](../../category-data/docs/README.md#release-manifest).

```rust
pub struct ReleaseKey { pub id: &'static str, pub public: [u8; 32] }
pub const CURRENT_KEY: Option<ReleaseKey> = None;
pub const NEXT_KEY: Option<ReleaseKey> = None;
#[cfg(feature = "test-key")] pub const TEST_KEY: ReleaseKey;    // id "test"
pub const MAX_VALIDITY_SECONDS: i64 = 60 * 24 * 60 * 60;          // 60 days
pub const SIGNATURE_BYTES: usize = 64;

pub fn keys() -> Vec<ReleaseKey>;
pub fn verify_manifest(manifest: &[u8], signature: &[u8], now: i64, loaded: Option<&str>)
    -> Result<Manifest, UpdateError>;
pub fn verify_stored(manifest: &[u8], signature: &[u8]) -> Result<Manifest, UpdateError>;
pub fn check_file(manifest: &Manifest, name: &str, bytes: &[u8]) -> Result<DomainTable, UpdateError>;
pub fn sha256_hex(bytes: &[u8]) -> String;
```

| Item | Guarantees |
|---|---|
| `keys()` | `CURRENT_KEY`, `NEXT_KEY` and, with `test-key`, `TEST_KEY`, in that order. |
| `verify_manifest` | In order: the signature is 64 bytes; `key_id` names one of `keys()`; ed25519 `verify_strict` over exactly the given bytes; `Manifest::parse`; `not_after >= now` and `not_after - now <= 60 days` (`now` in Unix seconds); version strictly newer than `loaded` when given (equal, older, or a non-numeric `loaded` is `NotNewer`). No I/O, never panics. |
| `verify_stored` | The same checks without expiry and version, for a release already on disk when the server starts. Expiry and rollback protect a fresh download against a replayed old release; files on disk were accepted with those checks when fetched and must keep loading after `not_after` while updates are off or the server is offline. |
| `check_file` | `name` is in `FILE_NAMES` and listed; `bytes` has the listed size (compared before hashing or decompressing) and SHA-256; it loads under the 32 MiB / 500 000-row caps; its header carries the table name fixed for `name` and the manifest's version; its data lines number the listed `rows`. Returns the loaded table. |
| `sha256_hex` | Lowercase hex SHA-256, as the manifest lists it. |
| `UpdateError` (`#[non_exhaustive]`) | `SignatureLength`, `NoReleaseKey` ("no release signing key is compiled in": the key id is unknown and both release slots are empty), `UnknownKey`, `Signature`, `Manifest(ManifestError)`, `Expired`, `TooFarAhead`, `NotNewer`, `FileName`, `NotListed`, `Size`, `Sha256`, `Data(DataError)`, `TableName`, `TableVersion`, `Rows`. `Display` names the rule only, never file content, so it can be logged. |

### Keys and rotation

Both release slots are empty: no production key pair exists yet, and an agent cannot make one safely. Until the maintainer fills `CURRENT_KEY`, every manifest not signed by the test key is refused with `NoReleaseKey`, so updates fail closed. The maintainer:

1. Runs `holtmail-category-builder keygen` on a trusted machine. It prints a key id, the 32-byte public key and the 32-byte seed.
2. Puts the id and public key into `CURRENT_KEY` in `src/manifest.rs` and releases the server.
3. Stores the seed as the GitHub Actions secret `CATEGORIES_SIGNING_KEY` (only the `sign` job of `.github/workflows/categories.yml` sees it) and keeps an offline copy. The seed never enters the repository.
4. Generates the next pair the same way and puts its public key into `NEXT_KEY` in a release, keeping its seed offline.
5. To rotate: once a release with `NEXT_KEY` has shipped, switch the secret to the next seed. Then, in a later release, move the next key into `CURRENT_KEY` and put a fresh next key into `NEXT_KEY`. A key that is compiled in but no longer signs keeps verifying stored releases until it is removed.

A compromised key is removed from both slots in a release; servers that have not updated still trust it until they do, and `not_after` (at most 60 days) bounds how long a captured manifest can be replayed to them.

### Feature `test-key`

Adds `TEST_KEY` (id `test`) to `keys()`. Its seed is committed at `tests/fixtures/test-signing-key.hex` (64 hex characters), so it is a test key, never a release key. `apps/server` forwards its `insights-test-data` feature to it, and `mise run release-features` fails when a release build enables it.

## Limits

- A table is at most 32 MiB decompressed and 500 000 rows (`holtmail-category-data` caps).
- The bundled file is at most 2 MiB compressed; a larger file fails the build (a compile-time check) and the tests.
- Memory: a sorted `Vec<(Box<str>, TopicSet)>` per table, about 6 MB at 132 000 rows. A reload holds the old and the new set while snapshots of the old one are alive.

## Data and licence

`data/wikidata-categories.tsv.gz` is CC0: Wikidata facts plus Holtmail curation. It is built by `apps/category-builder` and refreshed per release with `mise run categories-snapshot`, with a reviewed diff; always use that command so the compressed bytes compare like with like. UT1-derived data (CC BY-SA 4.0) is never bundled.

## Tests

```sh
cargo test -p holtmail-category-domains
cargo test -p holtmail-category-domains --features test-key
```

- `tests/conformance.rs` runs `holtmail_category::testing::conformance::categorizer` on two invented `example.com`/`example.org` tables (exact host before registrable domain, union across tables, case, no sender, unknown domain, no vote on own or forwarded mail) and on `with_bundled()`.
- Unit tests in `src/manifest/tests.rs` sign with the fixture seed and check through the internal key list in both builds: acceptance, short, long and wrong signatures, flipped manifest bytes, unknown and swapped keys, signed but invalid manifests, expiry at the boundaries, rollback and equal versions, unknown, unlisted and repeated file names, size and hash mismatch, header name, version and row count mismatch, an oversized file and a gzip bomb. `compiled_keys` checks the public functions: without the feature the `test` key id is refused with `NoReleaseKey`, with it the manifest is accepted.
- Unit tests in `src/tests.rs`: reload changes the version and answers of new snapshots only, duplicate names keep the old set, a concurrent reload never tears a snapshot, the bundled file parses as `wikidata` within `BUNDLED_MAX_BYTES`, `tables()` and the version format.
