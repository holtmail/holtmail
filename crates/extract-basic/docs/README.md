# holtmail-extract-basic

`BasicExtractor`, the [`Extractor`](../../extract/docs/README.md) Holtmail ships: plain text, CSV, HTML, PDF, DOCX, XLSX, PPTX, ODT, ODS and ODP, read with pure-Rust parsers so the server needs no system libraries.

## API

- `BasicExtractor::new()` — an extractor that gives up after `TIMEOUT` (30 s) per file.
- `TIMEOUT`, `MAX_UNPACKED` (128 MiB) — the limits below.

It keeps the contract's guarantees and passes its conformance suite.

## What it reads

| Type | Taken from |
| --- | --- |
| `text/*` except HTML, `application/json`, `application/xml`, CSV | The bytes as UTF-8 (mail parsing already decoded the declared charset); invalid bytes become `U+FFFD` |
| `text/html`, `application/xhtml+xml` | Text without tags, scripts or styles (mail-parser's `html_to_text`) |
| PDF | Each page's text with `lopdf`, page by page |
| DOCX | `word/document.xml`, footnotes and endnotes: `w:t` runs, a line per paragraph and table row |
| XLSX | Every `xl/worksheets/sheetN.xml` in order: shared strings, inline strings and values, a line per row |
| PPTX | Every `ppt/slides/slideN.xml` in slide order: `a:t` runs |
| ODT, ODS, ODP | `content.xml`: every text node, a line per paragraph, heading and table row |

For a generic type (`application/octet-stream`, `application/zip`, the various "download" types or none) the file name's extension decides: `txt`, `csv`, `tsv`, `md`, `log`, `json`, `xml`, `ics`, `vcf`, `html`, `htm`, `pdf`, `docx`, `xlsx`, `pptx`, `odt`, `ods`, `odp`. Anything else, including legacy `.doc`/`.xls`, RTF, images and `message/rfc822`, gives `None`.

The text is tidied: runs of white space become one space, empty lines go, then it is cut to `MAX_TEXT`.

## Limits

- Files over 25 MiB are refused before any parsing.
- Zip-based formats: at most 10 000 entries and 128 MiB unpacked across every part read, counted while inflating, so a zip bomb stops at the limit with `TooLarge`. At most 1 000 slides or sheets and a million shared strings.
- PDF: every stream, while loading and per page, may inflate to 64 MiB at most (`lopdf`'s limits); at most 5 000 pages. Encrypted PDFs are `Unreadable`. A page that fails is skipped; a document where every page fails is `Unreadable`.
- Parsing runs on Tokio's blocking pool inside `catch_unwind`, so a parser panic becomes `Unreadable`. After `TIMEOUT` the caller gets `TimedOut`; the blocking thread finishes its work in the background (a thread cannot be stopped), bounded by the limits above.
- Reading stops early once the text passes `MAX_TEXT`.

## Tests

`cargo test -p holtmail-extract-basic` runs the conformance suite and builds every fixture in code: PDFs with `lopdf`, office files as zips of hand-written XML, and a zip bomb of spaces.
