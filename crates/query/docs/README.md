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
| `has:attachment` | `HasAttachment` |
| `is:read` / `is:seen` | `Flag("\Seen")` |
| `is:unread` / `is:unseen` | `Not(Flag("\Seen"))` |
| `is:flagged` / `is:starred` | `Flag("\Flagged")` |
| `before:` / `after:` with `YYYY`, `YYYY-MM` or `YYYY-MM-DD` | `Before` / `After` at the start of that day, month or year in UTC |
| `-term` | `Not(term)` |

Operator names are case-insensitive. An unknown `name:value` (`re:hello`, a URL) is searched as plain text. An unclosed quote runs to the end of the input.

## Errors

`QueryError::InvalidDate(value)` for a date that is not `YYYY`, `YYYY-MM` or `YYYY-MM-DD`, and `QueryError::MissingValue(operator)` for `from:` with nothing after it. The web app shows both under the search box.
