# holtmail-import

Reads messages out of mailbox files, and address books and calendars out of `.vcf` and `.ics` files, so they can be ingested. Not a contract: each format has one right reading, built on the mbox and Maildir readers of [`mail-parser`](https://docs.rs/mail-parser).

## API

```rust
let messages = holtmail_import::open(path)?;   // detects the format
let messages = holtmail_import::open_as(path, Format::Mbox)?;
for message in messages {
    let ImportedMessage { folder, raw, flags, received_at } = message?;
}
```

Messages are read one at a time, in file order. An error for one message does not end the iteration; the caller records it and continues.

## Formats

| Format | Detected by | Folder | Flags | `received_at` |
| --- | --- | --- | --- | --- |
| `Mbox` | `.mbox`/`.mbx`, or a file starting with `From ` | File name without extension (`Sent.mbox` → `Sent`) | — | From the `From ` line |
| `Maildir` | A directory with `cur/`, or Maildir++ `.Folder` subdirectories | Maildir++ `.Work.Reports` → `Work/Reports`; `None` for the inbox | `S F R D T P` → `\Seen \Flagged \Answered \Draft \Deleted $Forwarded` | From the file name |
| `Eml` | `.eml` | `None` | — | — |
| `Zip` | `.zip` | The entry's directory (`Inbox/one.eml` → `Inbox`) | — | — |
| `Tree` | Any other directory | Subdirectory path; `.mbox` files add their name | — | mbox only |
| `VCard`, `ICalendar` | `.vcf`/`.vcard`, `.ics`/`.ical`/`.ifb` | No messages; see below | — | — |

## Address books and calendars

```rust
for file in holtmail_import::collections(path)? {
    let CollectionFile { path, name, format, bytes } = file; // format: CollectionFormat::{VCard, ICalendar}
}
```

`collections` returns every `.vcf`/`.vcard` and `.ics`/`.ical`/`.ifb` file in `path`, read whole and sorted by path: the file itself (`Format::VCard`, `Format::ICalendar`, which `open` reads as no messages), every such file in a directory tree, or every such entry of a zip, for example Google Takeout's `Takeout/Contacts/My Contacts/My Contacts.vcf` and `Takeout/Calendar/ada@example.com.ics`. `path` is the file's path inside what was opened with `/` between levels (the file name for a single file), `name` the file name without extension. Zip entries escaping the archive are skipped, as for mail. A file over `MAX_COLLECTION_FILE` (256 MiB) fails with `ImportError::TooLarge`. Mailbox formats give no files, and a zip or tree gives both its messages through `open` and its files through `collections`. Cutting the files into contacts and events is [`holtmail-pim`](../../pim/docs/README.md)'s job.

## Details

- mbox is read as mboxrd: a line `>From `, `>>From `, ... loses one `>`, and the empty line before the next `From ` line (`\n` or `\r\n`) belongs to the file, not the message, so it is dropped. A message written by `holtmail-export` therefore reads back to the same bytes, as long as it ended with a line break. Files imported before this kept that empty line, so importing the same mbox again stores those messages once more.
- A first line that is not a header, such as Outlook's `Microsoft Mail Internet Headers Version 2.0` or an mbox `From ` line in a single `.eml`, is removed before the message is returned.
- Zip entries are read through their enclosed name, so entries like `../escape.eml` are skipped.
- Other files in a tree or zip (`.txt`, attachments) are ignored.

Not yet: `.mbox` files inside zips, Google Takeout labels from `X-Gmail-Labels`, PST.
