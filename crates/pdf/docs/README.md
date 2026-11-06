# holtmail-pdf

A message as a PDF: its subject, header lines, plain text and attachment list on A4 pages, with the original message attached inside the PDF as `message.eml` (association `Source`, `message/rfc822`), so the PDF carries the raw bytes it was made from. A library on [krilla](https://crates.io/crates/krilla); [`holtmail-export`](../../export/docs/README.md) fills it from stored messages.

## API

- `MessagePdf { subject, headers, text, attachments, raw }`: header lines as `(label, value)` pairs in order, attachments as `(name, size)`.
- `render(&MessagePdf) -> Result<Vec<u8>, PdfError>`.
- `MAX_TEXT` (2 MiB): text past it is left out, with a line saying so; the attached original has all of it.

## Layout

A4, 56 pt margins, DejaVu Sans (regular and bold, from the `dejavu` crate, embedded and subset by krilla): subject 15 pt bold, header labels 9.5 pt bold in grey, values 9.5 pt, body 10 pt, 1.4 line height, a hairline under the headers and above the attachments, "n of m" at the foot of each page. Lines break at spaces and inside words longer than a line (URLs); tabs become four spaces and control characters are dropped. Widths come from the font's advances (`skrifa`), cached per character.

## Limits

- DejaVu Sans covers Latin, Greek, Cyrillic and more; characters it lacks (Chinese, Japanese, Korean, emoji) show as empty boxes.
- Text runs left to right; Hebrew and Arabic are not reordered.
- HTML is shown as the text derived from it; images and formatting are not drawn.
- Not PDF/A: the PDF is for reading and printing, the attached original is the record.

## Tests

`cargo test -p holtmail-pdf`: one and many pages with Cyrillic, the original read back unchanged from the PDF with `lopdf`, line widths inside the margin with long words and URLs, cleaning of tabs and control characters, and the note for a cut body.
