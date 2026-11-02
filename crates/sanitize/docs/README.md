# holtmail-sanitize

Makes message HTML safe to show in the reader. Built on [`ammonia`](https://docs.rs/ammonia). The reader also shows the result in a sandboxed iframe without scripts, so a mistake here still cannot run code.

## API

```rust
pub fn sanitize(html: &str, options: &Options) -> Sanitized;
pub fn document(body: &str, allow_remote: bool, inline_origin: &str) -> String;
```

`Options { allow_remote: bool, inline_parts: HashMap<String, String> }` and `Sanitized { html, remote_blocked }`.

## What `sanitize` does

- **Removes** scripts, event handlers, forms and inputs, frames, objects, embeds, comments, `javascript:` and relative URLs, `data:` anywhere except `<img src>` with an image type.
- **Keeps** what email layouts need: tables with `width`, `align`, `bgcolor`, `cellpadding` and friends, `<font>`, `<center>`, `class`, inline `style` attributes and `<style>` blocks.
- **Blocks remote content** unless `allow_remote`: `http(s)` and protocol-relative `src`, `srcset` and `background` are dropped; `url(...)` with a remote address in styles becomes `none`; `@import` is disabled. `remote_blocked` tells the reader to offer "Show remote content".
- **Rewrites inline parts**: `cid:` references become the absolute URLs from `inline_parts`, matched case-insensitively; unknown ones are dropped.
- **Links** get `target="_blank"` and `rel="noopener noreferrer"`.

## What `document` adds

A full page with `<meta charset="utf-8">`, `referrer: no-referrer`, `<base target="_blank">` and a Content Security Policy: `default-src 'none'`, images only from `inline_origin`, `data:` and `cid:`, inline styles only, no forms, no base changes. With `allow_remote`, `http:` and `https:` are added for images, styles and fonts.
