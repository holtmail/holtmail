# holtmail-ingest

The one path into the archive. Sync and import both hand raw message bytes to `Ingest::ingest`, which stores them, records metadata and indexes them, always in the same order and with the same rules.

## API

```rust
let ingest = Ingest::new(blobs, store, index) // Arc<dyn BlobStore>, Arc<dyn Store>, Arc<dyn SearchIndex>
    .with_extractor(extractor); // Arc<dyn Extractor>, optional
let done = ingest.ingest(archive, raw, &Placement { folder, uid, flags, labels, received_at }).await?;
ingest.commit().await?; // once per batch
ingest.reindex(message).await?; // after folders, flags or labels change
```

`Ingested { message_id, thread_id, created }`; `created` is `false` when the archive already had these bytes.

## What `ingest` does

1. Parses the bytes with `holtmail-mime`. Bytes that are not a message fail with `IngestError::Mime` before anything is written, so the caller can record the failure and move on.
2. Stores the raw bytes as a blob. They are durable before any metadata refers to them.
3. If the archive has no message with these bytes: stores every attachment as its own blob and inserts the message, which places it in a thread.
4. Records the location (`folder`, `uid`, `flags`) when the placement has a folder.
5. Replaces the message's labels when the placement has `labels: Some(..)`; `None` (imports, servers without labels) leaves them as they are.
6. Puts an index document built from the parsed message, every folder, flag and label the message currently has, and the text of its attachments.

Ingesting the same bytes again only adds or updates the location and refreshes the index document, so syncs and imports can be repeated safely.

## Attachment text

With `with_extractor`, every attachment that is not inline goes through the [`Extractor`](../../extract/docs/README.md) once per attachment blob, and the text is kept as a derived blob (`DerivedKind::Text` in the store). The same file in another message, a later `reindex` and flag or label changes reuse it instead of reading the file again. Files the extractor refuses (`TooLarge`, `Unreadable`, or no text at all) are recorded as `Derived::Nothing` and not tried again; unsupported types and `TimedOut` are not recorded, so a better extractor or a quieter moment picks them up. Extraction never fails ingest; only storage errors do. Without an extractor `attachment_text` is empty.

## Limits

- At most 1 MiB of body text is indexed per message; the stored message is never cut.
- At most `MAX_ATTACHMENT_TEXT` (4 MiB) of attachment text is indexed per message, all attachments together, each at most the extractor's 1 MiB. Attachments over 25 MiB are not read. Stored attachments are never cut.
- Extraction runs inline, so a sync waits for each file (bounded by the extractor's timeout).
- When threads merge, other messages' index documents keep their old thread id until reindexed. Search does not filter by thread, so results are unaffected.
