# holtmail-sanitize

Makes message HTML safe to show in the reader. Built on [`ammonia`](https://docs.rs/ammonia). The reader also shows the result in a sandboxed iframe without scripts, so a mistake here still cannot run code.

## API

```rust
pub fn sanitize(html: &str, options: &Options) -> Sanitized;
pub fn document(body: &str, allow_remote: bool, inline_origin: &str) -> String;
pub fn remote_images(html: &str) -> Vec<String>;
pub fn remote_key(url: &str) -> String;
```

`Options { remote: Remote, proxy_prefix: String, saved: HashSet<String>, inline_parts: HashMap<String, String> }` with `Remote::{Block (default), Proxy, Direct}`, and `Sanitized { html, remote_blocked, saved_shown }`; `saved_shown` is set when a URL was pointed at its saved copy.

- `remote_images(html)`: the absolute `http(s)` URLs of remote images, `background` attributes and style `url(...)`s, in document order, each once. Protocol-relative URLs become `https:`; `@import` targets are not included.
- `remote_key(url)`: BLAKE3 hex of the absolute URL, the name a remote image goes by in proxy URLs and in the store.

## What `sanitize` does

- **Removes** scripts, event handlers, forms and inputs, frames, objects, embeds, comments, `javascript:` and relative URLs, `data:` anywhere except `<img src>` with an image type.
- **Keeps** what email layouts need: tables with `width`, `align`, `bgcolor`, `cellpadding` and friends, `<font>`, `<center>`, `class`, inline `style` attributes and `<style>` blocks.
- **Remote content** in `src`, `background` (on `table`, `td`, `th`) and style `url(...)`:
  - a URL whose key is in `saved` becomes `proxy_prefix + key`, in every mode;
  - `Block` drops the rest: attributes go, `url(...)` becomes `none`, `@import` is disabled, and `remote_blocked` is set;
  - `Proxy` points the rest at `proxy_prefix + key` and still disables `@import`;
  - `Direct` keeps the original addresses.
- **Rewrites inline parts**: `cid:` references become the absolute URLs from `inline_parts`, matched case-insensitively; unknown ones are dropped.
- **Links** get `target="_blank"` and `rel="noopener noreferrer"`.

## What `document` adds

A full page with `<meta charset="utf-8">`, `referrer: no-referrer`, `<base target="_blank">` and a Content Security Policy: `default-src 'none'`, images only from `inline_origin`, `data:` and `cid:`, inline styles only, no forms, no base changes. With `allow_remote`, `http:` and `https:` are added for images, styles and fonts.
