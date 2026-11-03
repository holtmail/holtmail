# holtmail-index

The contract for full-text search. The index is derived data: it can be dropped and rebuilt from the store and the blobs at any time. Every search runs inside one archive.

## Documents

`IndexDocument` holds what search needs about one message: ids (`message_id`, `archive_id`, `thread_id`), `subject`, `from`, `recipients` (To, Cc and Bcc together), `body` (plain text), `attachment_names`, `attachment_text`, `folders` (paths and role names such as `inbox`), `flags`, `date` and `has_attachments`.

## Queries

A `SearchQuery` is a list of `Clause`s that must all match; an empty list matches everything. People type them through [`holtmail-query`](../../query/docs/README.md).

| Clause | Matches |
| --- | --- |
| `Text(words)` | Every word somewhere: subject, addresses, body, attachment names or text |
| `Phrase(words)` | The words next to each other, in order |
| `From`, `To` | Words in a sender's or recipient's name or address, as a phrase (`example.org` matches `ada@example.org`) |
| `Subject`, `Body` | Every word in that field |
| `Filename` | Words in an attachment name, as a phrase |
| `HasAttachment` | At least one attachment that is not inline |
| `In(folder)` | A folder path or role, case-insensitive |
| `Flag(flag)` | An exact flag, e.g. `\Seen` |
| `Before(t)`, `After(t)` | `date < t`, `date >= t` |
| `Not(clause)` | The inner clause does not match |

Words match after case folding and accent removal: `Zoë` finds `zoe`, `LUNCH` finds `lunch`. Outside phrases, a word of three letters or more also finds words that start with it: `quart` finds `quarterly`. Shorter words match whole words only.

## SearchIndex

| Method | Guarantees |
| --- | --- |
| `put(documents)` | Replaces documents with the same `message_id`; visible after `commit` |
| `remove(ids)` | Missing ids are ignored; visible after `commit` |
| `commit()` | Makes earlier changes visible and durable |
| `search(archive, query, sort, offset, limit)` | Hits in `Newest` or `Relevance` order and the total number of matches; never returns messages of another archive |

`IndexError::Invalid` for a query that cannot run (too many clauses), `IndexError::Backend` for failures that may succeed on retry.

## Writing an implementation

Run `holtmail_index::testing::conformance(Arc::new(index))` from a multi-threaded test with `holtmail-index = { workspace = true, features = ["testing"] }` in `[dev-dependencies]`.

## Implementations

- [holtmail-index-tantivy](../../index-tantivy/docs/README.md)
