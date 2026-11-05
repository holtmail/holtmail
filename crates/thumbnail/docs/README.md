# holtmail-thumbnail

Small preview images of image attachments. A plain library, not a contract: one pure function from bytes to a smaller image, with no I/O and nothing a deployment would want to swap. The API caches what it makes as blobs (see `DerivedStore` in [holtmail-store](../../store/docs/README.md)), so thumbnails are derived data that can be dropped and made again.

## API

- `thumbnail(source) -> Result<Thumbnail, ThumbnailError>` — fits the image in `MAX_SIDE` × `MAX_SIDE` (320 px), keeping its aspect ratio and turning it upright by its EXIF orientation. Smaller images keep their size; animated images give their first frame. Images with transparency come out as PNG, everything else as JPEG at quality 80. CPU-bound: run it off the async runtime. The same bytes always give the same answer.
- `Thumbnail { content_type, bytes }` — `image/jpeg` or `image/png`.
- `ThumbnailError::{Unsupported, TooLarge, Unreadable}`.
- `supports(content_type)` — whether a declared type (`image/png`, `image/jpeg`, `image/gif`, `image/webp`) is one worth offering a thumbnail for, without decoding anything.

## Limits

- Only PNG, JPEG, GIF and WebP are decoded, recognised by their bytes, whatever the message declared. SVG is never rendered.
- Sources over `MAX_SOURCE` (25 MiB) are refused.
- Width and height are read from the header first; more than `MAX_PIXELS` (40 million) or a side over 20 000 px is `TooLarge` before decoding. The decoder is also held to 256 MiB of memory.

## Dependencies

`image` with only its `png`, `jpeg`, `gif` and `webp` codecs (pure Rust: `png`, `zune-jpeg`, `gif`, `image-webp`).
