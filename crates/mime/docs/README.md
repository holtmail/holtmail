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
| `headers` | Every header field of the message itself as `HeaderField { name, value }`, in message order, repeated fields repeated: the name as written, the value unfolded to one line with encoded words decoded. Alias detection reads forwarding headers from it. `ParsedMessage::field_count(name)` counts the fields of a name, ignoring letter case |
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

## Delivery status notifications

```rust
pub fn parse_dsn(raw: &[u8]) -> Option<Dsn>;
pub fn parse_unstructured_bounce(raw: &[u8]) -> Option<Dsn>;
```

`parse_dsn` reads a delivery status notification (RFC 3464, and the internationalised RFC 6533 form) for bounce matching in the outbox. It takes any message with a `message/delivery-status` or `message/global-delivery-status` part of its own, normally inside `multipart/report`; parts of attached messages do not count, so a forwarded bounce is not read as one. Several status parts are read in order as one report. `None` when the bytes are not a message or carry no such part.

| Field | Meaning |
| --- | --- |
| `reporting_mta` | `Reporting-MTA` without its type (`dns;`) |
| `original_envelope_id` | `Original-Envelope-Id` as written, angle brackets removed, not xtext-decoded |
| `original_message_id` | `Message-ID` of the first attached original that has one (`message/rfc822`, `message/global`, `text/rfc822-headers`, `message/global-headers`), else `original_envelope_id`; no angle brackets |
| `recipients` | `DsnRecipient` per recipient group, in report order |

`DsnRecipient { final_recipient, original_recipient, action, status, diagnostic_code }`: addresses without their type and angle brackets, `utf-8` addresses decoded from `\x{HHHH}`; `action` is `DsnAction::{Failed, Delayed, Delivered, Relayed, Expanded}`, case-insensitive; `status` is `class.subject.detail` with class 2, 4 or 5 (`5.1.1`), trailing comments dropped; `diagnostic_code` is the text after its type (`smtp;`), unfolded to one line.

Malformed reports give what can be read and never panic: unknown fields (`X-Postfix-*`, `X-Display-Name`, `Remote-MTA`) are skipped, lines that are not fields are ignored, a group with no recipient field (`Final-Recipient`, `Original-Recipient`, `Action`, `Status`, `Diagnostic-Code`) is taken as per-message fields, and a broken value becomes `None` rather than dropping its recipient.

`parse_unstructured_bounce` is the fallback for bounces that are not reports: a message from a `mailer-daemon` or `postmaster` address (any domain, any letter case) with an attached original message that has a `Message-ID`. It gives that `Message-ID` and one recipient with only `action: Failed`; recipient, status and diagnostic stay `None`, because such bounces say neither reliably. It returns `None` for real reports, so callers try `parse_dsn` first. The human-readable text of a bounce is never scraped.

Fixtures (all invented): `dsn-postfix.eml` (Postfix with `text/rfc822-headers`), `dsn-exchange.eml` (Exchange with `message/rfc822`, folded diagnostic), `dsn-gmail.eml` (Gmail-shaped, nested alternative body), `dsn-delayed.eml` (`delayed`, no attachment, envelope id fallback), `dsn-multiple.eml` (global report with four recipients and every action), `dsn-malformed.eml` (broken base64 part, junk lines, bad values), `bounce-unstructured.eml` (legacy postmaster bounce). A test also feeds every truncated prefix of each fixture to both functions.
