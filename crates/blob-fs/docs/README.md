# holtmail-blob-fs

A [`BlobStore`](../../blob/docs/README.md) that keeps each blob as one file on a local disk. The default for single-machine installs.

## Usage

```rust
let store = FsBlobStore::open("/var/lib/holtmail/blobs").await?;
```

`open` creates the directory when it is missing.

## On disk

```
<root>/
  tmp/                  writes in progress
  ab/cd/abcd…           blob abcd… (64 hex characters)
```

Two levels of two-character directories keep any single directory to a few thousand entries even with tens of millions of messages.

## Guarantees and limits

- A blob is written to `tmp/`, `fsync`ed and renamed into place, so a crash never leaves a partial file under a blob id. Files left in `tmp/` after a crash are garbage and can be removed while the store is stopped.
- `get` re-hashes the file and fails with `BlobError::Corrupt` when it was changed on disk.
- `<root>` and `<root>/tmp` must be on the same filesystem, since rename is what makes a write atomic.
- Blobs are not compressed. Compression is a separate concern for a wrapping store.
