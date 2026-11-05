# holtmail-extract

The contract for turning attachments into plain text for the search index. The text is derived data: it can be cut short and made again from the attachment's bytes at any time, and the stored attachment is never changed. Ingest calls an `Extractor` for every attachment that is not inline and caches the result as a blob (see `DerivedStore` in [holtmail-store](../../store/docs/README.md)).

## Extractor

```rust
#[async_trait]
pub trait Extractor: Send + Sync {
    async fn extract(&self, file: &File) -> Result<Option<String>, ExtractError>;
}
```

`File { content_type, filename, content }`: the lowercase media type without parameters, the file name the message gave and the decoded bytes.

`extract(file)` guarantees:

- `Ok(None)` when the type is not one the extractor reads. The type comes from `content_type`, or from the file name's extension when the type is generic (`application/octet-stream`).
- At most `MAX_TEXT` (1 MiB) of UTF-8, cut at a character boundary.
- A file over `MAX_INPUT` (25 MiB) fails with `TooLarge` without being read; so does one that would unpack to more than the implementation allows.
- A damaged, encrypted or mislabelled file fails with `Unreadable`, never panics and never hangs the caller; slow work ends with `TimedOut`.
- The same input gives the same answer, except `TimedOut`, so callers may record `TooLarge` and `Unreadable` and not try again.
- Safe to call concurrently.

`cut(text, max)` cuts a string at a character boundary; implementations use it to keep the limit.

## Writing an implementation

Enable the `testing` feature in `[dev-dependencies]` and call `holtmail_extract::testing::conformance(&extractor)` from a test. The suite checks plain text, the file-name fallback, unknown types, both limits, determinism and that garbage labelled as PDF, DOCX or ODT gives no text. It needs no network and no fixture files.

## Implementations

- [holtmail-extract-basic](../../extract-basic/docs/README.md) — pure Rust: plain text, CSV, HTML, PDF, DOCX, XLSX, PPTX, ODT, ODS, ODP
