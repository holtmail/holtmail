# holtmail-sse

Parses a Server-Sent Events stream (the WHATWG `text/event-stream` format), as JMAP push sends it (RFC 8620 §7.3). No I/O: the caller feeds the bytes as they arrive and gets the events they complete. Used by [`holtmail-source-jmap`](../../source-jmap/docs/README.md) and [`holtmail-collection-jmap`](../../collection-jmap/docs/README.md), which may not depend on each other.

## API

| Item | Guarantees |
| --- | --- |
| `Parser::new(limit)` / `Parser::default()` | A parser that refuses lines and event data longer than `limit` bytes (1 MiB by default) |
| `Parser::push(chunk)` | The events `chunk` completes, in order. Lines may end in CRLF, LF or CR and may be split anywhere across chunks, also between CR and LF and inside a UTF-8 sequence. Comment lines (`:`) and unknown fields (`id`, `retry`, others) are skipped; a space after the colon is dropped; several `data` lines are joined with `\n`; an event without `data` is not dispatched. `TooLarge` past the limit; the parser should then be dropped |
| `Event { event, data }` | `event` is the `event` field, `message` when the server sent none |

## Tests

Unit tests in `src/lib.rs`: named and default events with every line end, a line split across chunks and inside a UTF-8 sequence, CRLF split between chunks, events without data and unknown fields, and an oversized line.
