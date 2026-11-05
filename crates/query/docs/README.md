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
| `before:` / `after:` with `YYYY`, `YYYY-MM` or `YYYY-MM-DD` | `Before` / `After` at the start of that day, month or year in UTC |
| `-term` | `Not(term)` |

Plain words and phrases search attachment text too, alongside the subject, addresses, body and file names; `attachment:` searches only inside attachments, and `filename:` only their names. `has:attachment` keeps messages with any attachment that is not inline and combines with both: `has:attachment budget` finds messages with attachments that mention "budget" anywhere, `attachment:budget` those whose attachments do.

Operator names are case-insensitive. An unknown `name:value` (`re:hello`, a URL) is searched as plain text. An unclosed quote runs to the end of the input.

## Errors

`QueryError::InvalidDate(value)` for a date that is not `YYYY`, `YYYY-MM` or `YYYY-MM-DD`, and `QueryError::MissingValue(operator)` for `from:` with nothing after it. The web app shows both under the search box.
