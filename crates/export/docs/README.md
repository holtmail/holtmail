# holtmail-export

Writes archived messages out as one mbox file or as a zip of `.eml` files: a whole archive, one folder, one label or the result of a search. Writes contacts and calendar items as one `.vcf` or `.ics` file of their latest versions, or every version as a zip (see [Contacts and calendars](#contacts-and-calendars)). Each message is written from its original stored bytes, unchanged, one at a time, so an archive far larger than memory exports with about one message held at once. Not a contract: like `holtmail-import`, each format has one right writing. It receives the store, blob store and index contracts, so it works the same over SQLite and S3.

## API

```rust
let exporter = Exporter::new(blobs, store).with_index(index);   // the index only for searches
let export = exporter.prepare(archive_id, Selection::Folder(folder_id), Format::Zip).await?;
let name = export.file_name();                                   // "Personal - INBOX.zip"
let totals = export.write(&mut out, &|t| eprintln!("{} messages", t.messages)).await?;
```

| Item | Guarantees |
| --- | --- |
| `Format::{Mbox, Zip}` | `parse("mbox" \| "zip")` in any case, `extension()`, `content_type()` (`application/mbox`, `application/zip`) |
| `Selection::{All, Folder(id), Label(name), Search(query)}` | Which messages: every one, those located in the folder (removed from the server or not), those carrying the label, those matching the search |
| `Exporter::prepare(archive, selection, format)` | Checks before anything is written: `NotFound("archive")`, `NotFound("folder")` for an unknown folder or one of another archive, `NoIndex` for a search without an index |
| `Export::file_name()` | Archive name, then ` - folder path`, ` - label` or ` - search` when narrowed, and the extension; one safe path segment |
| `Export::write(out, progress)` | Writes to any `AsyncWrite` front to back and flushes it, newest message first, calling `progress` after each page of 500. Messages whose bytes are missing, or no longer match their hash (`BlobStore::get` refuses those), are left out and counted in `Totals::skipped`. Any other failure, including the reader going away, stops it with an error and leaves `out` incomplete |
| `Totals` | `messages` written, their original `bytes`, `skipped` |
| `ExportError` | `NotFound`, `NoIndex`, `InvalidSearch` (from the index), `Store`, `Index`, `Blob`, `Io` |

## mbox

mboxrd, as the mbox reader of `holtmail-import` (mail-parser) reads it:

- Each message starts with `From <sender> <date>`: the first `From` address without spaces, or `MAILER-DAEMON`, and the message's date in UTC in `asctime` form (`Fri Mar 14 09:00:00 2025`).
- Every line of the message that starts with `From `, after any number of `>`, gets one more `>` in front.
- After the message comes an empty line in the message's own line ending (`\r\n` after a message ending in `\r\n`, else `\n`); a message without a final line break gets a line break and the empty line.

Importing the file gives back the same bytes for every message that ended with a line break, so re-importing an export into an archive that holds its messages adds nothing. A message without a final line break comes back with one; use zip when that matters. mbox keeps no folders.

## zip

- One `.eml` entry per message with its bytes unchanged, deflated, its modification time set to the message's date (dates before 1980 get the zip default).
- Entry names are `<directory>/<YYYY-MM-DD HHMM> <subject> <first 16 hex of the blob id>.eml`. The blob id part keeps names unique within an archive, where every message has its own bytes, and the same across exports.
- The directory is the path of the folder the message was found in, with `/` between levels; when the archive has more than one account, the account's name comes first. A message in several folders goes once, under the first of them in the store's folder order; a message in no folder sits at the top. With `Selection::Folder` every message goes under that folder.
- Every name segment is made safe for common file systems: control characters and `/ \ : * ? " < > |` become `_`, leading dots and trailing dots and spaces go, `.` and `..` become `_`, Windows device names (`CON`, `NUL`, `COM1`, ...) get a `_` in front, and segments are cut at 80 characters (60 for subjects). No entry can climb out of the directory it is unpacked into.
- The zip is written as a stream (sizes in data descriptors; zip64 for archives over 4 GiB is left to the `zip` crate and not tested here), so it needs no seeking and can go straight to an HTTP response. Each entry is compressed on a blocking thread.

`holtmail-import` reads it back with each directory as a folder.

## Contacts and calendars

```rust
let export = exporter
    .prepare_pim(archive_id, PimSelection { kind: PimKind::Contacts, collection: None, person: Some(id), range: None, removed: false }, PimFormat::Text)
    .await?;
let name = export.file_name();                                   // "Personal - contacts - Ada Quill.vcf"
let totals = export.write(&mut out, &|t| eprintln!("{} items", t.items)).await?;
```

| Item | Guarantees |
| --- | --- |
| `PimKind::{Contacts, Calendar}` | Contacts of the archive's address books, or every item (events, tasks, others) of its calendars, the "From mail" calendar included |
| `PimFormat::{Text, Zip}` | `parse("vcf" \| "ics" \| "text" \| "zip", kind)` (`vcf` only for contacts, `ics` only for calendars), `extension(kind)`, `content_type(kind)` (`text/vcard; charset=utf-8`, `text/calendar; charset=utf-8`, `application/zip`) |
| `PimSelection { kind, collection, person, range, removed }` | `collection`: one address book or calendar; `person`: a person's cards (contacts only); `range: (from, to)`: events whose instances reach into the span by their stored series span (`starts_at < to` and `ends_at > from`, endless series included), tasks and undated items left out (calendars only); `removed`: a text export includes items removed from their source (a zip always has them) |
| `Exporter::prepare_pim(archive, selection, format)` | Checks before anything is written: `NotFound("archive")`, `NotFound("collection")` for an unknown one or one of the other kind or archive, `NotFound("person")` for a person of another archive, `Invalid` for a range with contacts, a person with calendars, or a range that ends before it starts |
| `PimExport::file_name()` | `<archive> - contacts` or ` - calendar`, then ` - <collection>`, ` - <person's name>` or ` - range` when narrowed, and the extension; one safe path segment |
| `PimExport::write(out, progress)` | Streams to any `AsyncWrite` and flushes it, calling `progress` after each item. Items in collection order, then by title. Versions whose bytes are missing or damaged, and JSON versions that cannot be read for a text export, are counted in `PimTotals::skipped`; any other failure stops it |
| `PimTotals` | `items`, `versions`, `bytes`, `skipped` |

**Text.** A `.vcf` is the latest version of each card, one after another: vCards byte for byte, with a CRLF added when one does not end in a line break, so importing it into another archive gives the same blobs. An `.ics` is one `VCALENDAR` written through `holtmail_pim::IcsWriter`: an iCalendar item contributes the components of its latest version byte for byte, without its own calendar lines and `METHOD`, with its `VTIMEZONE`s once per `TZID` (the first definition wins); an item from mail is written from its merged view, the one its page shows (the latest full invitation, every attendee's last answer, `CANCELLED` once cancelled), since its latest version is often only a `REPLY` or `CANCEL`; Graph and JMAP items from their normalized view. Importing the `.ics` gives the same items with the same components, but not the same blobs: `holtmail-import` stores each item under the calendar lines of the file it came from, and those are now the export's (`VERSION`, Holtmail's `PRODID`, `CALSCALE`), so importing into the archive it came from adds a version to each native item. A second export and import of that copy is byte-identical. Contacts from Microsoft Graph and JMAP are stored as JSON and written through `holtmail_pim::as_text`: JSContact by `calcard`, Graph contacts as vCard 4.0 from the normalized model with the Graph `id` as `UID`, Graph events and JSCalendar objects as iCalendar 2.0 from the model (changed instances carry what they inherit from the series; times in the zone the source gave, its IANA name when known, without `VTIMEZONE`). What the model does not hold (pictures, categories, alarms) is not in those; the zip keeps the JSON itself.

**Zip.** Every version as it was stored: `<collection>/<title> <item id>/<n> <YYYY-MM-DD HHMMSS seen>.<vcf|ics|json>` (seen time in UTC), each entry dated by when the version was seen, and `manifest.json` last: `{ format: "holtmail-pim-export", version: 1, archive, kind, exported_at, collections: [{ id, name, directory, account_id, removed_at }], items: [{ id, collection_id, remote_id, kind, title, uid, first_seen_at, removed_at, directory, versions: [{ number, file, seen_at, content_type, blob_id, etag }] }] }`. Collection directories get the collection's id after the name when two names are alike or a name is `manifest.json`; names are made safe as for mail.

## Tests

`cargo test -p holtmail-export` exports an invented archive (CRLF and LF messages, `From ` and `>From ` lines in bodies, a subject with `../`, a message without a final line break) through SQLite and the file blob store, reads both formats back with `holtmail-import` and compares the blob ids and folders; narrows by folder, label and search; checks the refusals, the file name and that missing bytes are skipped. Unit tests cover the calendar dates, mbox quoting and endings, and safe names. `tests/pim.rs` ingests invented vCards (two versions of one, one removed), a Graph contact and iCalendar events and checks the `.vcf` (latest bytes unchanged, a line end added, the Graph card as a vCard), the person and address book selections, the zip's names, bytes and manifest, the calendar range and the refusals; the `.ics` as one `VCALENDAR`, an invitation, its reply and its cancellation from mail (fixtures in `tests/fixtures/`) written as the merged event without `METHOD`, and a re-import that keeps each event and its component bytes and gives the same bytes on a second round.
