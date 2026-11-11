# holtmail-query

Parses what people type into the search box into a [`SearchQuery`](../../index/docs/README.md), and tests a parsed query against one message without the index ([Matcher](#matcher)).

```text
from:ada@example.com has:attachment before:2024-01-01 "quarterly report" -in:trash
```

## Language

| Input | Clause |
| --- | --- |
| `word` | `Text` |
| `"several words"` | `Phrase` |
| `from:` `to:` `subject:` `body:` `filename:` `in:` with a word or `"quoted value"` | The field clause |
| `attachment:invoice` / `attachment:"draft copy"` | `Attachment`: words in the text extracted from attachments only |
| `label:Work` / `label:"Muy importante"` | `Label`: the whole label name, any letter case |
| `has:attachment` | `HasAttachment` |
| `is:read` / `is:seen` | `Flag("\Seen")` |
| `is:unread` / `is:unseen` | `Not(Flag("\Seen"))` |
| `is:flagged` / `is:starred` | `Flag("\Flagged")` |
| `is:event` (`is:events`, `is:meeting`), `is:contact` (`is:contacts`), `is:mail` (`is:message`) | `Kind`: only documents of that kind; the API asks for mail and for contacts and events separately with the same query |
| `has:code`, `has:link`, `has:unsubscribe`, `has:reservation`, `has:order`, `has:invoice`, `has:parcel`, `has:pass` | `Insight(name)`: an insight of that kind at `Likely` or higher, outside forwarded parts |
| `category:travel` (any topic: `purchases`, `travel`, `finance`, `health`, `social`, `jobs`, `events`, `government`) | `Category("topic:<name>")` |
| `category:bulk` (any kind: `personal`, `list`, `bulk`, `notification`) | `Category("kind:<name>")` |
| `is:newsletter` (`is:newsletters`), `is:notification` (`is:notifications`), `is:personal`, `is:list` | `Category("kind:bulk")`, `kind:notification`, `kind:personal`, `kind:list` |
| `with:ada` / `with:"Bob Stone"` / `with:bob@example.org` | `With`: mail from or to them, events they organize or attend, their contact cards |
| `before:` / `after:` with `YYYY`, `YYYY-MM` or `YYYY-MM-DD` | `Before` / `After` at the start of that day, month or year in UTC |
| `-term` | `Not(term)` |

Plain words and phrases search attachment text too, alongside the subject, addresses, body and file names; `attachment:` searches only inside attachments, and `filename:` only their names. `has:attachment` keeps messages with any attachment that is not inline and combines with both: `has:attachment budget` finds messages with attachments that mention "budget" anywhere, `attachment:budget` those whose attachments do.

Operator names and the values of `has:`, `is:` and `category:` are case-insensitive. An unknown `name:value` (`re:hello`, a URL), and an unknown `has:` or `is:` value (`has:dog`), is searched as plain text.

**Meaning change (Phase 40).** Before, `has:code`, `is:newsletter`, `category:travel` and the other new values were searched as plain text (the words `has` and `code`). They now select by derived insight data, which lags behind new mail while insights or the index catch up; see [RELEASES.md](../../../docs/RELEASES.md). An unclosed quote runs to the end of the input.

## Errors

`QueryError::InvalidDate(value)` for a date that is not `YYYY`, `YYYY-MM` or `YYYY-MM-DD`, `QueryError::MissingValue(operator)` for `from:` with nothing after it, and `QueryError::UnknownValue { operator, value }` for a `category:` value that is neither a topic nor a kind (`category: "weather" is not a category; use one of purchases, ...`). The web app shows both under the search box.

## Matcher

`Matcher` tests a parsed query against one message without the search index, for rules (`query` conditions in [WRITE.md](../../../docs/WRITE.md#flows)) and anything else that has to decide about new mail one message at a time.

```rust
let matcher = Matcher::parse("from:shop.example.com has:attachment -in:trash")?;
match matcher.test(&facts) {
    Verdict::Match => {}
    Verdict::NoMatch => {}
    Verdict::Unknown => {}
}
```

- `Matcher::new(SearchQuery)` or `Matcher::parse(&str)` (the same errors as `parse`); `query()` gives the query back.
- `test(&MessageFacts) -> Verdict` is pure and cheap; an empty query matches every message. `Verdict::is_match()` is true only for `Match`.
- **Agreement guarantee.** For a message whose facts are complete, `test` returns `Match` exactly when `SearchIndex::search` finds the message for the same query over an `IndexDocument` built from the same facts. It applies the index's rules itself: words split on everything that is not a letter or digit, lowercased and folded to ASCII (`Zoë` is `zoe`, `Straße` is `strasse`; Cyrillic is kept); outside phrases a word of three letters or more finds words that start with it, shorter ones whole words only; `Text`, `Subject`, `Body` and `Attachment` words also find other English and Russian forms through the same Snowball stemmers (`frostem`, the one Tantivy uses); `From`, `To`, `With` and `Filename` with several words are phrases; a phrase of one word is a prefix without other forms; a value with no words at all (`from:"!!"`) matches everything; `In`, `Label`, `Insight` and `Category` compare whole values ignoring case, `Flag` exactly; `Before` is strictly earlier, `After` at or later; `is:mail` matches, `is:event` and `is:contact` never do. The test in `tests/agreement.rs` runs about a hundred queries on an invented corpus against `holtmail-index-tantivy` (a dev-dependency only) and fails on the first disagreement.

### MessageFacts

What the store already holds about one message: `message_id`, `thread_id`, `accounts` (whose folders hold it), `subject`, `from` (plus the real sender behind an alias, as the index has it), `to`, `cc`, `bcc`, `headers` (decoded name and value pairs in message order; `header(name)` reads them ignoring case), `body`, `attachments` (`AttachmentFacts`: `filename`, `content_type`, `size`, `inline`), `attachment_text`, `folders` (paths and roles), `flags`, `labels`, `date` (the date lists sort by), `size`, `insights` and `categories` (the names the index holds: `invoice`, `kind:bulk`, `topic:travel`).

Only non-inline attachments count for `has:attachment` and `filename:`, as in the index. `headers`, `accounts`, `thread_id`, `size` and the attachment types and sizes are there for rule conditions; no query clause reads them yet.

### What the matcher cannot decide alone

Every clause of the language is decided from the facts; none needs the index. What can be missing is a fact, and an `Option` field says so: `None` means not known yet, `Some` of an empty value means known to be empty.

| Fact | `None` when | Clauses that become `Unknown` |
| --- | --- | --- |
| `body` | text not extracted yet | `body:`; words and phrases not already found in the subject, addresses or file names |
| `attachment_text` | attachment text not extracted yet | `attachment:`; words and phrases not already found elsewhere |
| `insights` | insights not derived yet | `has:code`, `has:invoice` and the other insight values |
| `categories` | categories not derived yet | `category:`, `is:newsletter`, `is:notification`, `is:personal`, `is:list` |

Clauses combine with Kleene logic: `-term` of an unknown term stays unknown, and a query is `Unknown` unless another clause is already `NoMatch`. The matcher never guesses; the caller decides what `Unknown` means. Rules take it as no match and log it with the message id (the draft id on `message.sending`); the executor hands a message to flows only after ingest derived its insights, categories and attachment text, so live mail lacks a fact only when deriving it failed (`Clause::reads_insights` tells which queries need them).

### Known differences from the index

- Folding to ASCII covers letters with accents and compatibility forms (`ﬁ`, full-width letters) and `ß æ œ ø ł đ ð þ ı ħ ŧ ŋ`; Tantivy's table folds a few rarer symbols the matcher keeps as they are.
- The index refuses more than 64 clauses in one search; the matcher tests any number.
- The index truncates very long bodies when indexing (ingest); the matcher reads whatever `body` it is given.
