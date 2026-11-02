# holtmail-ingest

The one path into the archive. Sync and import both hand raw message bytes to `Ingest::ingest`, which stores them, records metadata and indexes them, always in the same order and with the same rules.

## API

```rust
let ingest = Ingest::new(blobs, store, index); // Arc<dyn BlobStore>, Arc<dyn Store>, Arc<dyn SearchIndex>
let done = ingest.ingest(archive, raw, &Placement { folder, uid, flags, received_at }).await?;
ingest.commit().await?; // once per batch
ingest.reindex(message).await?; // after folders or flags change
```

`Ingested { message_id, thread_id, created }`; `created` is `false` when the archive already had these bytes.

## What `ingest` does

1. Parses the bytes with `holtmail-mime`. Bytes that are not a message fail with `IngestError::Mime` before anything is written, so the caller can record the failure and move on.
2. Stores the raw bytes as a blob. They are durable before any metadata refers to them.
3. If the archive has no message with these bytes: stores every attachment as its own blob and inserts the message, which places it in a thread.
4. Records the location (`folder`, `uid`, `flags`) when the placement has a folder.
5. Puts an index document built from the parsed message and every folder and flag the message currently has.

Ingesting the same bytes again only adds or updates the location and refreshes the index document, so syncs and imports can be repeated safely.

## Limits

- At most 1 MiB of body text is indexed per message; the stored message is never cut.
- Attachment text extraction (PDF, office files) is not done yet; `attachment_text` stays empty.
- When threads merge, other messages' index documents keep their old thread id until reindexed. Search does not filter by thread, so results are unaffected.
