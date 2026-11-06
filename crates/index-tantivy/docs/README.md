# holtmail-index-tantivy

A [`SearchIndex`](../../index/docs/README.md) on [Tantivy](https://docs.rs/tantivy), the Rust counterpart of Lucene.

## Usage

```rust
let index = TantivyIndex::open("/var/lib/holtmail/index")?;
let index = TantivyIndex::in_memory()?; // tests
```

`open` creates the directory and the index when missing. Tantivy cannot add fields to an existing index, so an index written by an older version (another schema, e.g. without `labels`) is deleted and replaced by an empty one; `was_emptied()` then answers `true` and the caller rebuilds it from the store (the server does this by itself, see [apps/server](../../../apps/server/docs/README.md)).

## How text is indexed

- Words are split on anything that is not a letter or digit, lowercased and folded to ASCII: `Zoë` → `zoe`, `ada@example.com` → `ada example com`. Cyrillic and other scripts are kept as they are, lowercased.
- `subject`, `from`, `recipients`, `body`, `filenames` and `attachment_text` are separate fields with positions, for field clauses and phrases; `all` holds all of them for free text.
- Folders and labels (lowercased) and flags are exact terms; `date` is a fast `i64` field for sorting and ranges.
- Contacts and events share the index: a stored `doc_type` term (`mail`, `event`, `contact`) tells them apart and every query is limited to messages or to items by it; items are keyed by a stored `item_id`, and their title, organizer, people and text go into `subject`, `from`, `recipients` and `body` (and `all`). Contacts have no `date`, so they sort after events. Adding `doc_type` and `item_id` changed the schema, so an index from before contacts and events were searchable is emptied on open and rebuilt by the server.

## Guarantees and limits

- `put` deletes any document with the same message id before adding, so it replaces.
- Changes become visible on `commit`, which also reloads the reader.
- Work runs on Tokio's blocking pool, so a large commit does not stall the async runtime.
- One writer per index directory: Tantivy locks it, so a second process fails to open it with `Backend` saying the index is in use by another process (a running `holtmail serve`).
- At most 64 clauses and 1000 hits per page.
- Search words are expanded at query time, so existing indexes need no rebuild. Outside phrases, a word of three letters or more matches words that start with it (`прив` finds `привет`), and `Text`, `Subject` and `Body` words also match by their English and Russian Snowball stem when it has four letters or more (`договора` finds `договор`, `repairing` finds `repairs`). Stems can over-match: `маяка` also finds `маяковский`.
- `From`, `To` and `Filename` match prefixes but are not stemmed; phrases match exact words.
- `attachment_hits`: when the query has `Text` or `Phrase`, a second query runs over the page's hits only (a term set of their ids), with those clauses on `subject`, `from`, `recipients`, `body` and `filenames` instead of `all`; hits it does not find matched only through `attachment_text`. Each word may still match in a different field, as with `all`. The cost is one small query per page.
