# holtmail-index

The contract for full-text search over messages, and over contacts and calendar events as documents of their own. The index is derived data: it can be dropped and rebuilt from the store and the blobs at any time. Every search runs inside one archive.

## Documents

`IndexDocument` holds what search needs about one message: ids (`message_id`, `archive_id`, `thread_id`), `subject`, `from`, `recipients` (To, Cc and Bcc together), `body` (plain text), `attachment_names`, `attachment_text`, `folders` (paths and role names such as `inbox`), `flags`, `labels`, `date` and `has_attachments`.

`ItemDocument` holds a contact or an event: `item_id`, `archive_id`, `kind` (`DocumentKind::Event` or `Contact`), `title` (summary or name), `organizer`, `people` (attendees, or a contact's own names and addresses), `text` (description and location, or organization, title, phones, notes and addresses) and `date` (an event's start; none for contacts). Messages and items live side by side: `search` only ever finds messages and `search_items` only items, with the same clauses: an item's title counts as the subject, its organizer as the sender, its people as recipients, its text as the body and its date as the date; clauses about folders, flags, labels and attachments never match items.

## Queries

A `SearchQuery` is a list of `Clause`s that must all match; an empty list matches everything. People type them through [`holtmail-query`](../../query/docs/README.md).

| Clause | Matches |
| --- | --- |
| `Text(words)` | Every word somewhere: subject, addresses, body, attachment names or text |
| `Phrase(words)` | The words next to each other, in order |
| `From`, `To` | Words in a sender's or recipient's name or address, as a phrase (`example.org` matches `ada@example.org`) |
| `Subject`, `Body` | Every word in that field |
| `Filename` | Words in an attachment name, as a phrase |
| `Attachment(words)` | Every word in `attachment_text`; words match as in `Text` |
| `HasAttachment` | At least one attachment that is not inline |
| `In(folder)` | A folder path or role, case-insensitive |
| `Flag(flag)` | An exact flag, e.g. `\Seen` |
| `Label(label)` | A whole label name, case-insensitive: `projects/2025` finds `Projects/2025`, `projects` does not |
| `Before(t)`, `After(t)` | `date < t`, `date >= t` |
| `Kind(kind)` | Documents of that kind: `Mail` every message and no item, `Event` or `Contact` only those items |
| `With(words)` | Words in the name or address of anyone involved, as a phrase: a message's sender or recipients, an event's organizer or attendees, a contact's own names and addresses |
| `Not(clause)` | The inner clause does not match |

Words match after case folding and accent removal: `Zoë` finds `zoe`, `LUNCH` finds `lunch`. Outside phrases, a word of three letters or more also finds words that start with it: `quart` finds `quarterly`. Shorter words match whole words only.

## SearchIndex

| Method | Guarantees |
| --- | --- |
| `put(documents)` | Replaces documents with the same `message_id`; visible after `commit` |
| `remove(ids)` | Missing ids are ignored; visible after `commit` |
| `put_items(documents)` | Replaces item documents with the same `item_id`; visible after `commit` |
| `remove_items(ids)` | Missing ids are ignored; visible after `commit`; messages stay |
| `commit()` | Makes earlier changes of both kinds visible and durable |
| `search(archive, query, sort, offset, limit)` | Hits in `Newest` or `Relevance` order and the total number of matches; never returns messages of another archive. `attachment_hits` lists the hits of the page that match only through `attachment_text` (without it they would not match), in hit order; empty when the query has no `Text` or `Phrase` |
| `search_items(archive, query, sort, offset, limit)` | Contacts and events (`ItemHit { item_id, kind }`) and their total; `Newest` lists events by start, the latest first, then contacts |

`IndexError::Invalid` for a query that cannot run (too many clauses), `IndexError::Backend` for failures that may succeed on retry.

## Writing an implementation

Run `holtmail_index::testing::conformance(Arc::new(index))` from a multi-threaded test (it checks messages, then items: kinds, `With`, field clauses on items, order, replacement and removal) with `holtmail-index = { workspace = true, features = ["testing"] }` in `[dev-dependencies]`.

## Implementations

- [holtmail-index-tantivy](../../index-tantivy/docs/README.md)
