# holtmail-compose

Turns drafts, replies and forwards into RFC 5322 bytes, built with [`mail-builder`](https://docs.rs/mail-builder). A pure library: no network, no files and no store. Attachments come in as bytes; the caller resolves blob ids. The bytes it returns are what the outbox stores and submits, so a retry sends the same message.

## API

```rust
pub fn build(draft: &Draft) -> Result<Built, ComposeError>;
pub fn reply(original: &Original, kind: ReplyKind, own: &[String], from: Mailbox) -> Result<Draft, ComposeError>;
pub fn forward(original: &Original, raw: &[u8], kind: ForwardKind, from: Mailbox) -> Draft;
pub fn reply_subject(subject: &str) -> String;
pub fn forward_subject(subject: &str) -> String;
pub fn strip_prefixes(subject: &str) -> &str;
pub fn shorten_references(references: &[String], max: usize) -> Vec<String>;
pub fn imip(from: Mailbox, to: Vec<Mailbox>, invitation: &Invitation, answering: Option<&Original>) -> Draft;
pub fn imip_subject(invitation: &Invitation) -> String;
pub fn imip_text(from: &Mailbox, invitation: &Invitation) -> String;
```

- `Mailbox { name, address }`.
- `Draft { from, to, cc, bcc, reply_to, subject, text, html, attachments, in_reply_to, references, headers, message_id_domain, date, calendar }`. `date: None` means now; `message_id_domain: None` means the domain of `from`; `calendar: Some(CalendarPart { method, bytes })` adds an iCalendar object as the last alternative (an iMIP message).
- `Attachment { name, content_type, bytes, content_id }`: a `content_id` makes it an inline part for `cid:` links in the HTML.
- `ExtraHeader { name, value }`, with `name` from `ALLOWED_HEADERS`: `Auto-Submitted` (values `no`, `auto-generated`, `auto-replied`, `auto-notified`, RFC 3834), `X-Auto-Response-Suppress`, `Precedence`, `Importance`, `X-Priority`. List headers such as `List-Unsubscribe` are refused: Holtmail does not run mailing lists.
- `Built { bytes, message_id, recipients }`.
- `Original { from, reply_to, to, cc, mail_followup_to, list_post, message_id, in_reply_to, references, subject, date, text, html }`, from `Original::from_parsed(&holtmail_mime::ParsedMessage)` or `Original::parse(raw)`.
- `ReplyKind::{Sender, All, List}`, `ForwardKind::{Inline, Attachment}`.
- `ComposeError::{Address(field), Injection(field), MessageId(field), Header(name), Attachment(index), Calendar, NoRecipients, NoListAddress}`. Messages name the field, never the value, so errors can be logged.

## What `build` guarantees

- **No header injection.** A CR, LF or NUL in any header value, display name, address, message id, extra header or attachment name is refused.
- **Addresses** are `local@domain`: a dot-atom local part in ASCII (quoted local parts, address literals and non-ASCII local parts are refused) and a domain written in its ASCII form (IDNA).
- **Encoding.** Non-ASCII display names and subjects become RFC 2047 encoded words; non-ASCII file names RFC 2231 parameters. Bodies are UTF-8 with CRLF line endings, `7bit` or quoted-printable as needed, and the bytes end with CRLF, as `Envelope::validate` needs.
- **Bcc** is never in the bytes. Without `To` and `Cc`, `To` is `undisclosed-recipients:;`.
- **Recipients** are `to`, then `cc`, then `bcc`, each address once, compared without regard to letter case, first spelling kept.
- **Body.** The HTML goes through `holtmail_sanitize::outgoing` (formatting, links and `cid:` images kept; scripts, forms and handlers removed; remote images left as they are) and is wrapped in a minimal UTF-8 page. The text alternative is the draft's text, or the sanitized HTML converted to text (`mail-parser`'s `html_to_text`) when the text is empty.
- **Structure.** `text/plain`, or `multipart/alternative` with HTML or a calendar part (text, HTML, then `text/calendar; method=...; charset=utf-8`, RFC 6047; the method must be one of `ITIP_METHODS` and the bytes UTF-8, else `Calendar`); inline parts make it `multipart/related` (only with HTML, otherwise they are ordinary attachments); other attachments make it `multipart/mixed`.
- **Attached messages.** `message/rfc822` attachments keep their bytes with bare LFs made CRLF, as `7bit` or `8bit`. When a line is longer than 998 bytes or the bytes contain NUL, the part is base64, which RFC 2046 does not allow for `message/rfc822` but every common reader opens.
- **Message-ID** is new on every call: `<uuid v7@domain>`. `Date` is written in UTC.

## Replies

- **Sender:** `Reply-To` when present, otherwise `From`. When those are all the user's own addresses (`own` or `from`), the reply goes to the original's `To`: answering one's own sent message continues the conversation.
- **All:** `Mail-Followup-To` when present. Otherwise the sender as above and the original's `To` in `To`, its `Cc` in `Cc`. Every address in `own` and the identity's address are removed; when `To` ends up empty, `Cc` moves to `To`.
- **List:** the `mailto:` address of `List-Post`; `NoListAddress` when it is missing or `NO`.
- Nobody appears twice, comparing addresses without regard to letter case.
- **Threading:** `In-Reply-To` is the original's id; `References` is the original's `References` (or its `In-Reply-To` when it has none) plus its id, shortened by `shorten_references` to `MAX_REFERENCES` (20): repeated ids dropped, then ids from the middle, keeping the first one (the thread's root) and the latest ones.
- **Subject:** exactly one `Re:`. `strip_prefixes` removes, in any case and order, `Re`, `AW`, `SV`, `Antw`, `Ответ`, `Отв`, `Fwd`, `FW`, `WG`, `TR` and `Enc` followed by a colon, with an optional counter (`Re[2]:`, `Re(3):`, `Re^2:`). A word without the colon (`Reunion`, `Re Ada`) is kept, and so are list tags such as `[dev]`.
- **Quoting:** the text body is an empty line for the answer, then `On <date> at <time> UTC, <sender> wrote:` and the original's text with `> ` before each line (`>` before lines already quoted and empty ones). When the original has HTML, the HTML body is an empty paragraph, then the attribution and the original's HTML, cleaned with the outgoing profile, inside `<blockquote type="cite">`; the reader's quote folding knows both forms. The attribution is in English; the web app may replace it before sending.

## Forwards

Recipients are empty and the subject gets exactly one `Fwd:`.

- **Inline:** a `---------- Forwarded message ----------` block with the original's `From`, `Date`, `Subject`, `To` and `Cc`, then its text (and its HTML, cleaned, when it has one). Its attachments and inline parts are attached again, read from `raw`.
- **Attachment:** `raw` attached unchanged as `message/rfc822`, named after the subject without prefixes (`Plans.eml`). This works for messages long deleted from the server.

## iMIP messages

`imip(from, to, invitation, answering)` is the draft of an iMIP message around an iTIP object made by `holtmail-pim-edit` (`reply`, `request`, `cancel`, `organize`). `Invitation { kind, calendar, summary, when, location, comment }` with `ImipKind::{Invitation, Update, Reply(Answer), Cancel}` and `Answer::{Accepted, Tentative, Declined}`; `ImipKind::method` is `REQUEST`, `REPLY` or `CANCEL`.

- **Subject** (`imip_subject`, English): `Invitation: <title>`, `Updated invitation:`, `Accepted:`, `Tentatively accepted:`, `Declined:`, `Cancelled:`; `(no title)` without one. Control characters in the title become spaces, so it can never break the header.
- **Text** (`imip_text`, English): one sentence (`Ada Quill accepted "Weekly sync".`), then `When:` and `Where:` when known, then the comment. The web app may replace it before sending, as with the reply attribution.
- **Threading:** with `answering` (the invitation mail a `REPLY` answers), `In-Reply-To` is its `Message-ID` and `References` follow the reply rules above.
- Nothing else: no HTML, no attachments, no extra headers; `build` turns the draft into bytes.

## Tests

`cargo test -p holtmail-compose`:

- Hand-written drafts in `tests/compose.rs` compared with the expected `.eml` files in `tests/fixtures/` after normalising `Message-ID` and MIME boundaries: plain text with non-ASCII names, HTML with an inline image, an attachment and `Auto-Submitted`, reply-all, inline forward and forward as attachment of the invented `original.eml`, and an iMIP `REPLY` (`imip-reply.eml`) answering the invented `invitation.eml`. Every output is parsed back with `mail-parser`. Run with `HOLTMAIL_BLESS=1` to rewrite the expected files after an intended change, then read the diff.
- iMIP subjects, a title with a line break, a cancellation without threading, calendar methods and bytes refused.
- Reply recipient rules (own messages, `Mail-Followup-To`, list replies, `Cc` promotion), threading, Bcc only, header injection and invalid input, long lines in attached messages.
- Property tests for subject prefixes (never piling up) and `References` shortening (root and latest kept, idempotent).
