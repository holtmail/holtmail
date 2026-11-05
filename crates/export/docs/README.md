# holtmail-export

Writes archived messages out as one mbox file or as a zip of `.eml` files: a whole archive, one folder, one label or the result of a search. Each message is written from its original stored bytes, unchanged, one at a time, so an archive far larger than memory exports with about one message held at once. Not a contract: like `holtmail-import`, each format has one right writing. It receives the store, blob store and index contracts, so it works the same over SQLite and S3.

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

## Tests

`cargo test -p holtmail-export` exports an invented archive (CRLF and LF messages, `From ` and `>From ` lines in bodies, a subject with `../`, a message without a final line break) through SQLite and the file blob store, reads both formats back with `holtmail-import` and compares the blob ids and folders; narrows by folder, label and search; checks the refusals, the file name and that missing bytes are skipped. Unit tests cover the calendar dates, mbox quoting and endings, and safe names.
