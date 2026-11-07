# holtmail-query

Parses what people type into the search box into a [`SearchQuery`](../../index/docs/README.md).

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
