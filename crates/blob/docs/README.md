# holtmail-blob

The contract for storing the raw bytes of archived messages. Every message is kept exactly as the server sent it, addressed by the BLAKE3 hash of its bytes, so a message seen in several folders or accounts is stored once. Parsed fields, search indexes and previews are derived from these bytes and can always be rebuilt.

## BlobId

```rust
pub struct BlobId([u8; 32]);

BlobId::of(bytes: &[u8]) -> BlobId
id.to_string()          // 64 lowercase hex characters
"…".parse::<BlobId>()   // Err(InvalidBlobId) unless 64 lowercase hex characters
```

Two blobs have the same id exactly when their bytes are equal.

## BlobStore

```rust
#[async_trait]
pub trait BlobStore: Send + Sync {
    async fn put(&self, bytes: Bytes) -> Result<BlobId, BlobError>;
    async fn get(&self, id: &BlobId) -> Result<Option<Bytes>, BlobError>;
    async fn contains(&self, id: &BlobId) -> Result<bool, BlobError>;
    async fn delete(&self, id: &BlobId) -> Result<(), BlobError>;
}
```

| Method     | Guarantees                                                                                              |
| ---------- | ------------------------------------------------------------------------------------------------------- |
| `put`      | Returns `BlobId::of(&bytes)`. Idempotent. Once it returns, the blob survives a crash.                   |
| `get`      | The exact stored bytes, `None` for a missing blob, `BlobError::Corrupt` instead of bytes that do not hash to the id. |
| `contains` | Whether the blob is stored.                                                                             |
| `delete`   | Removes the blob; deleting a missing blob succeeds.                                                     |

All methods may be called concurrently, including `put` of the same bytes from many tasks.

`BlobError` has two cases: `Corrupt(BlobId)` when stored bytes no longer match their id, and `Backend(..)` for any failure of the storage itself, which may succeed on retry.

## Writing an implementation

1. Create `crates/blob-<kind>` depending on `holtmail-blob`.
2. Implement `BlobStore`, mapping every backend error into `BlobError::Backend`.
3. Run the conformance suite from a multi-threaded test:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn keeps_the_blob_store_contract() {
    let store = MyBlobStore::open(/* empty store */).await.unwrap();
    holtmail_blob::testing::conformance(Arc::new(store)).await;
}
```

with `holtmail-blob = { workspace = true, features = ["testing"] }` in `[dev-dependencies]`.

## Implementations

- [holtmail-blob-fs](../../blob-fs/docs/README.md) — files on a local disk
- [holtmail-blob-s3](../../blob-s3/docs/README.md) — S3-compatible object storage
