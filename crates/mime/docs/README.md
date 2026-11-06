# holtmail-mime

Turns the raw bytes of a message into the fields Holtmail stores, shows and indexes: headers, addresses, threading ids, the text and HTML bodies, a one-line preview and attachments. Built on [`mail-parser`](https://docs.rs/mail-parser) with every charset enabled; no `mail_parser` type appears in the public API.

The raw bytes stay the truth: everything `parse` returns can be produced again from them, so a better parser later means re-parsing, not migrating.

## API

```rust
pub fn parse(raw: &[u8]) -> Result<ParsedMessage, MimeError>;
```

| Field | Meaning |
| --- | --- |
| `message_id`, `in_reply_to`, `references` | Ids without angle brackets; `references` oldest first |
| `subject` | Decoded, including RFC 2047 encoded words in any charset |
| `sent_at` | `Date` as Unix seconds in UTC; `None` when missing or broken |
| `from`, `to`, `cc`, `bcc`, `reply_to` | `Address { name, email }` from `holtmail-core`, groups flattened, entries without an address dropped |
| `text` | Plain body; derived from HTML when the message has no text part |
| `html` | HTML body as sent, **unsanitized**: never show it without `holtmail-sanitize` |
| `preview` | First `PREVIEW_CHARS` (200) characters of the body as one line |
| `attachments` | `Attachment { filename, content_type, content_id, inline, content }` in document order |
| `calendars` | The message's own `text/calendar` and `application/ics` parts (iMIP invitations, attached `.ics` files), inline or not, decoded: text parts as UTF-8 from their charset, others transfer-decoded. Parts of attached `message/rfc822` messages are not included. Each is also listed in `attachments` |

`content_type` is lowercase and defaults to `application/octet-stream`. Inline parts referenced by `cid:` links are attachments with `inline: true` and a `content_id`.

## Errors

`parse` is forgiving: broken encodings, unknown charsets and damaged MIME structure give partial fields, not errors. It fails with `MimeError::NotAMessage` only when the bytes have no header field at all.

## Tests

Fixtures in `tests/fixtures/` are invented messages: plain text, `multipart/alternative` with an attachment and threading headers, and legacy charsets (`windows-1251` encoded words, a `koi8-r` body).

`journal_original(raw)` returns the original message inside a Microsoft Exchange journal report (a message with an `X-MS-Journal-Report` header) byte for byte, as it was attached; `None` for anything else. The envelope part of the report (with Bcc recipients) is not part of it. Fixture `journal-report.eml` is invented.
