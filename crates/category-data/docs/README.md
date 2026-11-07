# holtmail-category-data

The category data file format (`docs/INSIGHTS.md` §8): a sorted, gzip-compressed TSV that maps registrable domains to topics, with a deterministic writer, a bounded reader and the in-memory `DomainTable` used for lookup, and the release manifest (`Manifest`) that lists the published files. It is the one implementation of the format, shared by `category-domains` (the categorizer that loads the bundled and downloaded files) and `apps/category-builder` (which writes them). It is a plain library, not a contract: no I/O beyond the byte slices it is given, no network, no logging; every function is deterministic, takes any input and never panics.

## Format

```
#holtmail-categories 1 <name> <version>\n
<domain>\t<topic>[,<topic>...]\t<source>\n
...
```

- **Header**, line 1: the magic `#holtmail-categories`, the format number `1` (`FORMAT`), the table name (`[a-z0-9-]{1,32}`, such as `wikidata`) and its version (`[A-Za-z0-9.-]{1,32}`), separated by single spaces.
- **Rows**, one per line, exactly three tab-separated columns, every line ending in `\n` (no `\r`, no empty lines):
  - `domain`: a lowercase ASCII A-label host, at least two labels of `[a-z0-9-]{1,63}` without a leading or trailing hyphen, at most 253 bytes, no trailing dot, a last label that is not all digits. Usually a registrable domain; a host is allowed for the exact-host lookup.
  - `topics`: one or more `Topic::name`s joined by `,`, written in bit order. Each name is `[a-z0-9_]{1,32}`.
  - `source`: where the row comes from, such as `wikidata:Q42`; 1 to 64 bytes (`MAX_SOURCE_BYTES`), no control characters (so no tab or newline). It is kept for the builder and not loaded into memory.
- **Sorting**: domains are strictly increasing in byte order, so a duplicate is refused like an unsorted key.
- **Compression**: gzip. The writer uses no file name, a zero timestamp and the best compression, so equal input gives equal bytes under the same build.
- **Limits**: the decompressed file is at most 32 MiB (`MAX_DECOMPRESSED_BYTES`) and has at most 500 000 rows (`MAX_ROWS`). Decompression stops one byte past the cap, so a gzip bomb never allocates past it.
- **Unknown topics**: a topic name this build does not know is skipped, so a file built for a newer taxonomy still loads on an older server. A row left with no known topic is dropped and counted (`DomainTable::skipped`). Any other bad line refuses the whole file.

A header-only file is a valid empty table.

## Public API

| Item | Guarantees |
|---|---|
| `pub const FORMAT: u32 = 1` | The format number in the header; any other is refused. |
| `pub const MAX_DECOMPRESSED_BYTES: u64` | 32 MiB, the default `Limits::max_bytes`. |
| `pub const MAX_ROWS: usize` | 500 000, the default `Limits::max_rows`. Rows dropped for unknown topics count. |
| `pub const MAX_SOURCE_BYTES: usize` | 64, the longest `source`. |
| `pub struct Header { pub name: String, pub version: String }`, `Header::new(name, version)` | The header line. Checked only when written or parsed. |
| `pub struct Row { pub domain: String, pub topics: TopicSet, pub source: String }` | One data row with its source. |
| `pub struct Limits { pub max_bytes: u64, pub max_rows: usize }` | Loading caps; `Default` is the two constants. |
| `pub fn is_valid_domain(&str) -> bool` | The key rule above. |
| `pub fn read(gz: &[u8], limits: Limits) -> Result<(Header, Vec<Row>), DataError>` | Every check of `parse_with`; rows with their source in file order, unknown topic names dropped, rows with no known topic left out. For the builder's previous-release and gate checks. |
| `pub fn parse(gz: &[u8]) -> Result<DomainTable, DataError>` | `parse_with` under the default limits. |
| `pub fn parse_with(gz: &[u8], limits: Limits) -> Result<DomainTable, DataError>` | Loads all or nothing: refuses bad gzip, a file over `max_bytes`, more than `max_rows` rows, invalid UTF-8, a bad header, a bad line, domain, topic list or source, and unsorted or duplicate keys, each line error with its 1-based line number (the header is line 1) and a static rule name. |
| `pub fn write(header: &Header, rows: &[Row]) -> Result<Vec<u8>, DataError>` | Validates exactly as `parse_with` does, and also refuses empty topic sets and topic bits this build does not know. Rows must already be sorted and unique. Deterministic bytes. `parse(write(h, rows))` gives back every row. |
| `pub struct DomainTable` | Immutable sorted `(domain, TopicSet)` rows with binary search; no source column in memory. |
| `DomainTable::header`, `name`, `version` | From the header line. |
| `DomainTable::len`, `is_empty` | Rows kept, each with at least one known topic. |
| `DomainTable::skipped` | Rows dropped because none of their topics is known. |
| `DomainTable::hash` | Lowercase hex BLAKE3 of the decompressed file: equal for equal content whatever the compression, different for different content. |
| `DomainTable::get(domain) -> Option<TopicSet>` | The topics of exactly this key, compared byte for byte. |
| `DomainTable::lookup(host: Option<&str>, registrable: Option<&str>) -> Option<TopicSet>` | The exact host first, then the registrable domain, both ASCII-case-insensitive. No Public Suffix List logic: the caller passes the registrable domain. |
| `pub enum DataError` (`#[non_exhaustive]`) | `Gzip`, `TooLarge`, `TooManyRows`, `Header(&'static str)`, `Line { line, rule }`. `Display` names only the rule and line number, never row text, so it can be logged. |

Header rule names: `missing`, `newline`, `utf8`, `magic`, `fields`, `format`, `name`, `version`. Line rule names: `newline`, `utf8`, `empty`, `fields`, `domain`, `topics`, `source`, `order`, `duplicate`.

## Release manifest

A data release (Phase 42, `docs/INSIGHTS.md` §8 Distribution) is `manifest.json` (`MANIFEST_NAME`), its detached signature `manifest.json.sig` (`SIGNATURE_NAME`: the 64 raw bytes of an ed25519 signature over the exact manifest bytes) and the files it lists. This crate owns the manifest's shape and rules; the signature check and the compiled-in keys are in `holtmail-category-domains`.

```json
{
  "format": 1,
  "version": "2026110801",
  "key_id": "release-2026",
  "not_after": "2027-01-06T00:00:00Z",
  "files": [
    {
      "name": "wikidata-categories.tsv.gz",
      "sha256": "<64 lowercase hex characters>",
      "size": 347649,
      "license": "CC0-1.0",
      "rows": 28370
    },
    {
      "name": "ut1-categories.tsv.gz",
      "sha256": "<64 lowercase hex characters>",
      "size": 1000,
      "license": "CC-BY-SA-4.0",
      "rows": 10
    }
  ]
}
```

| Field | Rule |
|---|---|
| `format` | `MANIFEST_FORMAT` (1). |
| `version` | 1 to 16 ASCII digits, compared as a number. Every listed file's header carries the same version. |
| `key_id` | `[a-z0-9-]{1,32}`, the signing key's id. |
| `not_after` | RFC 3339 UTC with whole seconds, exactly `YYYY-MM-DDTHH:MM:SSZ` (four-digit year); `Manifest::not_after` holds it as Unix seconds. |
| `files` | One or two entries, each file name at most once, in any order. |
| `files[].name` | One of `FILE_NAMES`: `wikidata-categories.tsv.gz` (table `wikidata`, `CC0-1.0`) or `ut1-categories.tsv.gz` (table `ut1`, `CC-BY-SA-4.0`). |
| `files[].sha256` | SHA-256 of the compressed file, 64 lowercase hex characters. |
| `files[].size` | Compressed size, 1 to 16 MiB (`MAX_FILE_BYTES`). |
| `files[].license` | The SPDX id `DATA_FILES` fixes for that name. |
| `files[].rows` | Data lines in the file, the header not counted (rows dropped for unknown topics count), at most `MAX_ROWS`. |

Unknown fields, at the top or in a file entry, are refused. The whole manifest is at most 64 KiB (`MAX_MANIFEST_BYTES`).

| Item | Guarantees |
|---|---|
| `pub const MANIFEST_FORMAT: u32 = 1`, `MAX_MANIFEST_BYTES: usize` (64 KiB), `MAX_FILE_BYTES: u64` (16 MiB) | The format number and caps above. |
| `pub const MANIFEST_NAME`, `SIGNATURE_NAME` | `manifest.json`, `manifest.json.sig`. |
| `pub const FILE_NAMES: [&str; 2]` | The closed set of data file names. |
| `pub struct DataFile { pub file, pub table, pub license }`, `pub const DATA_FILES: [DataFile; 2]` | File name to header table name to licence, for both files. |
| `pub fn data_file(file: &str) -> Option<DataFile>`, `pub fn data_file_for_table(table: &str) -> Option<DataFile>` | Exact lookup either way; `None` outside the set. |
| `pub fn parse_version(&str) -> Option<u64>` | `Some` for 1 to 16 digits (leading zeros allowed), else `None`. The one version rule for the builder, `category-domains` and the server. |
| `pub fn is_newer(new: &str, old: &str) -> bool` | `new > old` as numbers; false when either does not parse, so an unreadable version is never newer. |
| `pub struct Manifest { pub format: u32, pub version: String, pub key_id: String, pub not_after: i64, pub files: Vec<ManifestFile> }` | The fields above. |
| `pub struct ManifestFile { pub name: String, pub sha256: String, pub size: u64, pub license: String, pub rows: u64 }` | One listed file. |
| `Manifest::parse(&[u8]) -> Result<Manifest, ManifestError>` | Size cap before reading, strict JSON shape (no unknown, missing or mistyped field), then `check`. Never panics. |
| `Manifest::check(&self) -> Result<(), ManifestError>` | Every rule in the table, first failure reported in field order. |
| `Manifest::peek_key_id(&[u8]) -> Result<String, ManifestError>` | Only `key_id` (size cap and key id rule), to pick the key before the signature is checked; says nothing about the other fields. |
| `Manifest::file(&self, name) -> Option<&ManifestFile>` | The entry with that name. |
| `Manifest::to_json(&self) -> Vec<u8>` | Two-space indented JSON, fields in the order above, files in list order, one trailing newline. Deterministic, so the signature is made over exactly these bytes; `parse(to_json(m)) == m` for every `m` that passes `check`. It does not check. |
| `pub enum ManifestError` (`#[non_exhaustive]`) | `TooLarge`, `Json`, `Format`, `Version`, `KeyId`, `NotAfter`, `Files`, `FileName`, `DuplicateFile`, `Sha256`, `Size`, `License`, `Rows`. `Display` names the rule, never a value. |

The verifier checks the bytes it received, never a re-serialisation, so a manifest written by another JSON writer verifies as long as it is signed as written.

## Licensing

The format and this crate are code under the workspace licence (MIT OR Apache-2.0). The data files carry their own data licences, independent of the code: `wikidata-categories.tsv.gz` is CC0 and may be bundled; `ut1-categories.tsv.gz` is CC BY-SA 4.0, ships with `LICENSE-DATA` and the UT1 attribution, and is never bundled into the binary or committed (`docs/INSIGHTS.md` §8, Licensing).

## Limits and notes

- The compressed bytes depend on the deflate backend `flate2` is built with. In this workspace `zip` (`deflate-flate2-zlib-rs`) turns on the `zlib-rs` backend whenever it is in the same build, while a build without it (such as `cargo run -p holtmail-category-builder`) uses `miniz_oxide`; the decompressed text, and so `hash()`, is the same either way. Snapshots should be written by one command (`mise run categories-snapshot`) so a reviewed diff compares like with like.
- About 132k rows take about 6 MB in memory; loading also holds the decompressed file (at most 32 MiB) until the table is built.
