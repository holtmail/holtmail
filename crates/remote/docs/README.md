# holtmail-remote

Saves remote images linked from archived messages into the archive. Newsletters and notifications often show nothing but images hosted by the sender, and those disappear within months; a saved copy keeps the message looking the same years later, and shows without contacting anyone.

## API

```rust
let remote = RemoteImages::new(fetcher, blobs, store);
let saved = remote.save_archive(archive_id, Scope::New, &cancel, |saved| report(saved)).await?;
let retried = remote.retry_failed(archive_id, &cancel, |saved| report(saved)).await?;
```

`save_archive(archive, scope, cancel, progress)` looks at the messages that were not looked at yet, when the archive has saving on, and does nothing otherwise. `Scope::New` is mail archived since saving was turned on, run after every sync and import; `Scope::Older` is mail archived before, run only on request because an archive can hold years of mail.

`retry_failed(archive, cancel, progress)` fetches again every image of the archive that could not be saved, once each.

Both return `Saved { total, messages, fetched, known, failed, cancelled }` (`messages` counts images for `retry_failed`), call `progress` after every step and check `cancel` between steps; work done before a cancel stays done. `save_archive` looks at up to `MESSAGES_AT_ONCE` (4) messages of a batch of 50 together, so one slow sender does not hold up the others; on a cancel it starts no further message and returns once the started ones are done.

Guarantees:

- **Each URL once.** A URL fetched, or failed, before (for any archive) is linked without fetching it again, also when messages looked at together link it. URLs are compared by their [`remote_key`](../../sanitize/docs/README.md).
- **Each image once.** Bytes go to the blob store, which addresses them by BLAKE3 hash, so the same image behind several URLs is stored once.
- **Bytes first.** The image is durable before any message links to it.
- **Failures are recorded.** Refused URLs (private addresses, not an image, too large) and unreachable ones are stored with a short reason and are not fetched again by `save_archive`; `retry_failed` tries them on request.
- **Messages stay as they are.** The original bytes are never changed; copies are separate blobs linked to the message.
- **Incremental.** Messages are marked once looked at, so a second run only sees messages added since. Running while mail is being added is safe.

Errors are `RemoteError::Store` or `RemoteError::Blob`; a failed image fetch is never an error.

## Limits

- At most 100 images per message (`MAX_IMAGES_PER_MESSAGE`); six downloads at a time across the messages looked at together. A slow image still holds up the end of its batch of 50 messages.
- Only the HTML body is looked at: `<img src>`, `background` and style `url(...)`. Styles pulled in with `@import` are not saved.
- Downloading tells the sender the message was fetched, from the server's address. That is why saving is off by default and turned on per archive.

## Tests

`a_slow_sender_does_not_hold_up_other_messages` holds one image until the other five messages (100 images and one URL they share) are done, and checks that the shared URL is fetched once and no more than six downloads ran at once. `saves_each_image_once_and_links_every_message` runs against SQLite, the filesystem blob store and an in-process fake fetcher: a turned-off archive is skipped, known URLs are not fetched again, identical bytes share a blob and failures are recorded.
