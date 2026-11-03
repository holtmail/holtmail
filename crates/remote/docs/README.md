# holtmail-remote

Saves remote images linked from archived messages into the archive. Newsletters and notifications often show nothing but images hosted by the sender, and those disappear within months; a saved copy keeps the message looking the same years later, and shows without contacting anyone.

## API

```rust
let remote = RemoteImages::new(fetcher, blobs, store);
let saved = remote.save_archive(archive_id).await?;
```

`save_archive(archive)` looks at every message of the archive that was not looked at yet, when the archive has `save_remote_images` on, and does nothing otherwise. It returns `Saved { messages, fetched, known, failed }`.

Guarantees:

- **Each URL once.** A URL fetched, or failed, before (for any archive) is linked without fetching it again. URLs are compared by their [`remote_key`](../../sanitize/docs/README.md).
- **Each image once.** Bytes go to the blob store, which addresses them by BLAKE3 hash, so the same image behind several URLs is stored once.
- **Bytes first.** The image is durable before any message links to it.
- **Failures are recorded and not retried.** Refused URLs (private addresses, not an image, too large) and unreachable ones are stored with a short reason.
- **Incremental.** Messages are marked once looked at, so a second run only sees messages added since. Running while mail is being added is safe.

Errors are `RemoteError::Store` or `RemoteError::Blob`; a failed image fetch is never an error.

## Limits

- At most 100 images per message (`MAX_IMAGES_PER_MESSAGE`); six downloads at a time.
- Only the HTML body is looked at: `<img src>`, `background` and style `url(...)`. Styles pulled in with `@import` are not saved.
- Downloading tells the sender the message was fetched, from the server's address. That is why saving is off by default and turned on per archive.

## Tests

`saves_each_image_once_and_links_every_message` runs against SQLite, the filesystem blob store and an in-process fake fetcher: a turned-off archive is skipped, known URLs are not fetched again, identical bytes share a blob and failures are recorded.
