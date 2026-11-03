# holtmail-index-tantivy

A [`SearchIndex`](../../index/docs/README.md) on [Tantivy](https://docs.rs/tantivy), the Rust counterpart of Lucene.

## Usage

```rust
let index = TantivyIndex::open("/var/lib/holtmail/index")?;
let index = TantivyIndex::in_memory()?; // tests
```

`open` creates the directory and the index when missing.

## How text is indexed

- Words are split on anything that is not a letter or digit, lowercased and folded to ASCII: `Zoë` → `zoe`, `ada@example.com` → `ada example com`. Cyrillic and other scripts are kept as they are, lowercased.
- `subject`, `from`, `recipients`, `body`, `filenames` and `attachment_text` are separate fields with positions, for field clauses and phrases; `all` holds all of them for free text.
- Folders (lowercased) and flags are exact terms; `date` is a fast `i64` field for sorting and ranges.

## Guarantees and limits

- `put` deletes any document with the same message id before adding, so it replaces.
- Changes become visible on `commit`, which also reloads the reader.
- Work runs on Tokio's blocking pool, so a large commit does not stall the async runtime.
- One writer per index directory: Tantivy locks it, so a second process fails to open it.
- At most 64 clauses and 1000 hits per page.
- Search words are expanded at query time, so existing indexes need no rebuild. Outside phrases, a word of three letters or more matches words that start with it (`прив` finds `привет`), and `Text`, `Subject` and `Body` words also match by their English and Russian Snowball stem when it has four letters or more (`договора` finds `договор`, `repairing` finds `repairs`). Stems can over-match: `маяка` also finds `маяковский`.
- `From`, `To` and `Filename` match prefixes but are not stemmed; phrases match exact words.
