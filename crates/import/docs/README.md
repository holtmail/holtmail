# holtmail-import

Reads messages out of mailbox files so they can be ingested. Not a contract: each format has one right reading, built on the mbox and Maildir readers of [`mail-parser`](https://docs.rs/mail-parser).

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

## Details

- mbox is read as mboxrd: a line `>From `, `>>From `, ... loses one `>`, and the empty line before the next `From ` line (`\n` or `\r\n`) belongs to the file, not the message, so it is dropped. A message written by `holtmail-export` therefore reads back to the same bytes, as long as it ended with a line break. Files imported before this kept that empty line, so importing the same mbox again stores those messages once more.
- A first line that is not a header, such as Outlook's `Microsoft Mail Internet Headers Version 2.0` or an mbox `From ` line in a single `.eml`, is removed before the message is returned.
- Zip entries are read through their enclosed name, so entries like `../escape.eml` are skipped.
- Other files in a tree or zip (`.txt`, attachments) are ignored.

Not yet: `.mbox` files inside zips, Google Takeout labels from `X-Gmail-Labels`, PST.
