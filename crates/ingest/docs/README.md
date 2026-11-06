# holtmail-ingest

The one path into the archive. Sync and import both hand raw message bytes to `Ingest::ingest`, which stores them, records metadata and indexes them, always in the same order and with the same rules. Contacts, events and tasks go through `Ingest::ingest_item` the same way.

## API

```rust
let ingest = Ingest::new(blobs, store, index) // Arc<dyn BlobStore>, Arc<dyn Store>, Arc<dyn SearchIndex>
    .with_extractor(extractor) // Arc<dyn Extractor>, optional
    .with_alias_detectors(detectors) // Vec<Arc<dyn AliasDetector>>, optional
    .with_insights(runner); // Arc<holtmail_insights::Runner>, optional
let done = ingest.ingest(archive, raw, &Placement { folder, uid, flags, labels, received_at }).await?;
ingest.commit().await?; // once per batch
ingest.reindex(message).await?; // after folders, flags or labels change
ingest.refresh_alias(message).await?; // detect the alias again; true when it changed
ingest.reindex_parsed(message, &parsed).await?; // the index document from bytes already parsed
ingest.refresh_insights(message).await?; // compute insights again; true when the row changed
```

`Ingested { message_id, thread_id, created }`; `created` is `false` when the archive already had these bytes.

## What `ingest` does

1. Parses the bytes with `holtmail-mime`. Bytes that are not a message fail with `IngestError::Mime` before anything is written, so the caller can record the failure and move on. Bytes whose hash is a tombstone of the archive (deleted there on purpose, see [`DeletionStore`](../../store/docs/README.md#deletionstore)) fail with `IngestError::Deleted` before anything is stored; a placement with a folder and UID is recorded as a deleted location, so the source is not asked for it again.
2. Stores the raw bytes as a blob. They are durable before any metadata refers to them.
3. If the archive has no message with these bytes: stores every attachment as its own blob and inserts the message, which places it in a thread.
4. Records the location (`folder`, `uid`, `flags`) when the placement has a folder.
5. Replaces the message's labels when the placement has `labels: Some(..)`; `None` (imports, servers without labels) leaves them as they are.
6. Puts an index document built from the parsed message, every folder, flag and label the message currently has, and the text of its attachments.

Between steps 3 and 4, a new message's alias is recorded (see [Aliases](#aliases)), its calendar parts become events (see [Invitations in mail](#invitations-in-mail)) and its insights are computed (see [Insights](#insights)).

Ingesting the same bytes again only adds or updates the location and refreshes the index document, so syncs and imports can be repeated safely.

## Contacts and events

```rust
let done = ingest.ingest_item(IncomingItem { collection, remote_id, kind, format, raw, etag, seen_at }).await?;
ingest.rebuild_item(item).await?; // the item's PimIndex again: latest raw version (read in its stored format), or every version merged for items from mail; then derive_item
ingest.derive_item(item).await?;  // its calendar instances and search document from the stored PimIndex
let index = holtmail_ingest::pim_index(&normalized, version); // what the store lists items by
```

`ingest_item` reads the raw object in its `format` (`holtmail_pim::Format`: vCard, iCalendar, or the JSON of Graph or JMAP, stored as the version's content type) with [`holtmail-pim`](../../pim/docs/README.md), stores a picture embedded in a vCard as a blob of its own (the same picture in many cards is one blob), stores the raw bytes unchanged, then calls `PimStore::record_version` with the normalized fields. A version is added only when the bytes differ from the item's latest; bytes that cannot be read are archived as an unreadable version. It returns the store's `RecordedVersion`. The item's key is the vCard or iCalendar `UID`.

Whenever an item's normalized fields change (a new version, `rebuild_item`, a new version from mail), `derive_item(item)` rebuilds what is derived from them: its part of the calendar (`holtmail_calendar::series` stored through `CalendarStore::replace_series`: its group of copies by `UID`, rank fields and instances, see [`holtmail-calendar`](../../calendar/docs/README.md)) and its search document (`item_document(summary)`: events and contacts as `ItemDocument`s, `put_items`; tasks, other items and unreadable ones are removed with `remove_items`). Like messages, item documents become searchable after `Ingest::commit`; `holtmail-pim-sync`, the server's import and `reindex` commit after their items. `MAIL_COLLECTION` now lives in `holtmail-calendar` and is re-exported here.

## Invitations in mail

```rust
let added = ingest.record_invitations(message).await?; // the stored message's calendar parts again
```

Every `text/calendar` or `application/ics` part of a message (`ParsedMessage::calendars`: iMIP invitations, updates, replies, cancellations, and attached `.ics` files) is cut by `holtmail_pim::split`; each `VEVENT` or `VTODO` with a `UID` becomes a version of an item in the archive's "From mail" calendar (`MAIL_COLLECTION`, one per archive, no account). The item's `remote_id` is the `UID` (`task:<UID>` for tasks), so the series and its changed instances are one item. The version records the part's `METHOD` and is ordered by `SEQUENCE`, `DTSTAMP` and the message's `Date` through `PimStore::record_mail_version`, which also links the message. After a new version the item's `PimIndex` is rebuilt from every version merged by `holtmail_pim::merge`: the latest invitation, every attendee's latest answer, and `CANCELLED` once cancelled. `ingest` does this for new messages only; `record_invitations` reads a stored message again for mail archived before this existed, and is idempotent. `rebuild_item` merges the same way for items from mail.

## Aliases

With `with_alias_detectors`, the first detector (in the given order) that recognises a new message decides its alias, recorded with `AliasStore::set_message_alias` (see [holtmail-alias](../../alias/docs/README.md) and [ALIASES.md](../../../docs/ALIASES.md)); a message no detector recognises is recorded as having none. The sender a service hid is added to the index document's `from`, so `from:` finds the message by either address; `reindex` keeps it. `detect_alias(&parsed)` is the detection alone. `refresh_alias(message)` reads the stored bytes, detects again, records the answer and reindexes the message only when the answer changed; the server runs it over every message when the detectors' versions change.

## Insights

With `with_insights`, message insights (stage A of [INSIGHTS.md](../../../docs/INSIGHTS.md), run by [`holtmail-insights`](../../insights/docs/README.md)) are computed from the parsed message and stored with `InsightStore::set_message_insights` under `runner.extract_fp()`. The runner runs inside `holtmail_insights::with_message`, so a panicking extractor's report names the message id.

- `ingest` computes them for a new message, and for known bytes whose message has no `message_insights` row, so a repeated sync repairs a lost write. Known bytes that already have a row write nothing. Failing to read or store the row is logged with the message id only and never fails ingest.
- `reindex` computes and stores them before building the document when the row is missing or its fingerprint is not the current one.
- `refresh_insights(message)` reads the stored bytes and writes only when the row changed in anything but `computed_at` (items, signals, kinds, auth facts, sender address, outcome, `truncated`, fingerprint). It returns whether it wrote. No indexed field depends on insights yet, so it does not reindex.
- For the server's walk, which reads and parses each blob itself: `insights_for(message, &raw, &parsed)` computes and stores like `refresh_insights` and returns whether it wrote; `mark_insights_unreadable(message)` and `mark_insights_unparsable(message)` write a row with that outcome, no items, no signals or kinds and the current fingerprint, so the message is not listed as needing insights again until the fingerprint changes. Both fail with `StoreError::NotFound` for an unknown message.

Without a runner nothing about insights is read or written, `refresh_insights` and `insights_for` return `false`, and the marks do nothing. Stage B (categories) is not run here yet.

## Attachment text

With `with_extractor`, every attachment that is not inline goes through the [`Extractor`](../../extract/docs/README.md) once per attachment blob, and the text is kept as a derived blob (`DerivedKind::Text` in the store). The same file in another message, a later `reindex` and flag or label changes reuse it instead of reading the file again. Files the extractor refuses (`TooLarge`, `Unreadable`, or no text at all) are recorded as `Derived::Nothing` and not tried again; unsupported types and `TimedOut` are not recorded, so a better extractor or a quieter moment picks them up. Extraction never fails ingest; only storage errors do. Without an extractor `attachment_text` is empty.

## Limits

- At most 1 MiB of body text is indexed per message; the stored message is never cut.
- At most `MAX_ATTACHMENT_TEXT` (4 MiB) of attachment text is indexed per message, all attachments together, each at most the extractor's 1 MiB. Attachments over 25 MiB are not read. Stored attachments are never cut.
- Extraction runs inline, so a sync waits for each file (bounded by the extractor's timeout).
- When threads merge, other messages' index documents keep their old thread id until reindexed. Search does not filter by thread, so results are unaffected.

`refresh_attachment_text(message, which)` reads the attachments `which` picks again, ignoring the kept text: it forgets their derived text, deletes text blobs nothing else uses, and rebuilds the index document. The server uses it once to replace raw calendar and vCard lines with their words.
