# holtmail-html-scan

Turns a message's HTML into the `HtmlView` that insight extractors read (visible anchors, visible text segments with emphasis and line flags, raw `ld+json` blocks, microdata items and the footer marker), and finds the inline-forward marker in a plain-text body. A plain library on the html5ever **tokenizer**: no DOM is built, so memory stays linear in the input and nothing recurses over it. The tree-building rules that decide what ends up inside what are emulated with an explicit element stack. `holtmail-insights` calls it once per message while building `InsightInput`. The design is in [INSIGHTS.md](../../../docs/INSIGHTS.md) §7.0 and §6.1.

## Public API

### `scan(html: &str) -> holtmail_insight::HtmlView`

Guarantees:

- **Input.** Reads at most `limits::MAX_HTML_BYTES` (2 MiB), cut on a char boundary. The rest is ignored. The caller caps too and sets `truncated`.
- **Deterministic, never panics, bounded.** Equal input gives equal output. Memory is linear in the input, stack use is constant, and the crate denies `clippy::indexing_slicing` and `clippy::string_slice`.
- **Tokenizer states as in a browser.** The sink switches the tokenizer as the html5ever tree builder does:

  | Element | State |
  |---|---|
  | `title`, `textarea` | RCDATA |
  | `style`, `xmp`, `iframe`, `noembed`, `noframes`, `noscript` | RAWTEXT (scripting on, as the sanitizer parses) |
  | `script` | script data |
  | `plaintext` | PLAINTEXT, to the end of the input |

  None of these ever give a segment or anchor text. Inside SVG and MathML, `style` and `script` are ordinary elements and `<![CDATA[` is text, as in foreign content.
- **Hidden subtrees are skipped**: no anchors and no segments come from them.
  - Attributes: `hidden`, and `aria-hidden="true"`.
  - Inline styles: `display:none`, `visibility:hidden|collapse`, `font-size:0`, `opacity:0`, and `max-height:0` together with `overflow(-y):hidden`. The last declaration wins, and `!important` is ignored.
  - Elements: `template`, `noscript`, `object`, `applet`, `select`, `datalist`, all SVG and MathML content, and HTML elements named like an SVG or MathML element (`<text>`, `<g>`, `<desc>`, `<set>` and others). The sanitizer drops those last ones together with their content.
  - Skipped text separates words: `48<span hidden>99</span>2913` gives `48 2913`, never `482913`.
- **Anchors** (`limits::MAX_ANCHORS`, 500, in document order):
  - Only a visible `<a>` whose `href` parses as an absolute `http`, `https` or `mailto` URL is reported. `href` is reported as written, with entities decoded.
  - `text` is the visible text inside, with whitespace collapsed. `aria_label` is the attribute as written.
  - `index` is the position among the reported anchors.
  - `button_like` is set by `role=button`, by the anchor's own background or padding, or by a background, `bgcolor` or padding on its nearest enclosing `td`/`th`.
  - A nested `<a>` closes the open one, as in a browser.
- **Segments** (`limits::MAX_SEGMENTS`, 4,000):
  - Runs of visible text with whitespace collapsed. Soft hyphens and zero-width spaces, non-joiners and BOMs are dropped.
  - A segment ends at a line boundary (block elements, `td`/`th`, `br`, `hr`) and where emphasis changes.
  - `emphasis` comes from `strong`, `b`, `h1`–`h3`, `font-weight` bold or ≥ 600, a positive `letter-spacing`, a large `font-size` (≥ 20px, 15pt, 1.25em/rem, 125%, or `large` and up), or `<font size>` ≥ 5 or `+2`. An element passes emphasis to everything inside it.
  - `alone_on_line` is set when the segment is the only one on its line.
- **Forward.** The first segment that starts a line and begins with an inline-forward marker (the rule of `forward_at`) and every later segment get `below_forward`. Anchors that open at or after that segment get it too.
- **Footer.** The footer starts at the first unsubscribe anchor or the first visible `©`, whichever comes first. An unsubscribe anchor is one whose href contains `unsubscribe`, or whose text or label holds an unsubscribe phrase in English, German, French, Russian, Spanish, Dutch, Italian or Portuguese.
  - `footer_at` is the index of the segment where the marker falls. It can equal `segments.len()` when nothing follows the marker.
  - Anchors from the marker on get `in_footer`. The unsubscribe anchor itself is one of them; an anchor that is already open when the `©` appears is not.
- **`ld_json`.** Holds the text of each `<script type="application/ld+json">` block, unparsed (media-type parameters and letter case are ignored).
  - Blocks are collected whether CSS-hidden or not, and in `<head>` too, but never inside `template` or foreign content.
  - At most `limits::MAX_LD_JSON_BLOCKS` (8) are kept. A block larger than `limits::MAX_LD_JSON_BYTES` (256 KiB) is dropped whole, never cut. Either sets `truncated_structured`.
  - `ld_json_forward_at` is the index of the first block that starts below the inline-forward marker (the same marker that sets `below_forward` on segments and anchors). Blocks are in document order, so the blocks below the marker are a suffix of the list.
- **`microdata`** holds the WHATWG microdata items, flat, in the document order of their `itemscope` elements.
  - **Where it is read.** Microdata is metadata, like JSON-LD, and senders often hide it, so it is collected inside CSS-hidden and `aria-hidden` subtrees too. It is never collected inside `template`, foreign content (SVG, MathML), raw-text elements (`script`, `style`, `textarea` and the others above) or elements the sanitizer drops with their content.
  - **Items.** `itemscope` creates an item: `types` from `itemtype` split on ASCII whitespace, `id` from `itemid` as written. `itemscope` on `<html>` or `<body>` makes the item that every property outside other items belongs to; `itemprop` and `id` on those two are ignored. `top_level` is set when the element has no `itemprop` names.
  - **Properties.** `itemprop` holds space-separated names; each name is one property of the nearest enclosing item, with the same value. On an element that is also an item, the value is `Item(index)` of that item. Otherwise:

    | Element | Value |
    |---|---|
    | `meta` | `Text(content)` |
    | `audio`, `embed`, `iframe`, `img`, `source`, `track`, `video` | `Url(src)` |
    | `a`, `area`, `link` | `Url(href)` |
    | `object` | `Url(data)` |
    | `data`, `meter` | `Text(value)` |
    | `time` | `Text(datetime)`, else its text content |
    | anything else | `Text` of its text content |

    A missing attribute gives an empty value. URLs are as written; one longer than `limits::MAX_URL_BYTES` (8 KiB) is left out, never cut. Text is trimmed and cut like display text (`truncate_display`, 512 chars). Text content is the text of the whole subtree, hidden text included, with whitespace collapsed; line boundaries and skipped raw text become a space. `itemprop` on other raw-text elements (`title`, `textarea`, `noscript`, ...) is ignored.
  - **`itemref`.** Resolved after the whole input is read, without recursion. An `itemref` token names the first element with that `id`; the item gains the properties in that element's subtree (the element's own `itemprop` included) whose nearest item is not inside that subtree, as the WHATWG crawl does. Each id counts once per item (a visited set), a property already on the item is not added again, and a property whose value is the item itself is cut. Missing ids are ignored. Other `Item` cycles through `itemref` can remain; every `Item(index)` is inside the list.
  - **Budgets.** At most `limits::MAX_MICRODATA_PROPERTIES` (4,000) properties in all, counted while expanding `itemref`; at most 4,000 items; item nesting at most `limits::MAX_MICRODATA_DEPTH` (16), where an `itemscope` deeper than that is a plain element, so its `itemprop` takes its text content and its descendants' properties go to the nearest kept item; and at most 64,000 property visits while expanding `itemref`, so a fan-in of many items onto one large subtree stays linear. Elements past the 256-deep element stack keep attribute values but get no text-content values and no items. Hitting any of these sets `truncated_structured`.
  - `below_forward` on an item is set when its `itemscope` element starts at or after the inline-forward marker.
- **`truncated_structured`** is set whenever a JSON-LD or microdata cap left something out. The view's list lengths alone do not show it.
- **Sanitizer agreement.** Every reported `href`, and every word of every segment, also survives `holtmail_sanitize::sanitize` on the same input (see Tests).

### `forward_at(text: &str) -> Option<usize>`

The char offset of the first inline-forward marker in a plain-text body (the CONVERSATIONS §6.3 list).

- The marker must start a line, with leading whitespace allowed. It is one of:
  - `Forwarded message`, `Weitergeleitete Nachricht`, `Message transféré` or `Пересылаемое сообщение`, with at least two dashes before the phrase (Gmail, Thunderbird, Yandex);
  - `Begin forwarded message` (Apple Mail).
- Letter case is ignored. The offset is that of the line's first non-whitespace char.
- Prose such as "the forwarded message" and reply headers such as `-----Original Message-----` are not markers.
- Linear in the input and never panics.

### `VERSION: &str`

`"2"` (Phase 37: microdata, `ld_json_forward_at` and `truncated_structured`). It changes whenever the same input could give a different view or offset, and it is part of `extract_fp`.

## Limits and deliberate choices

- **Depth.** The element stack holds 256 elements. Deeper elements are flat: they are not pushed and do not change emphasis or button context, and anchors that open past the cap are not reported. A hidden element past the cap hides everything until the stack shrinks again. An inert one (`template`, foreign content, a dropped name) hides everything to the end of the input, so a hidden subtree is never unhidden early.
- **Tree-builder emulation.** The scan does not build a DOM. It emulates the rules that decide nesting:
  - implied `p`, `li`, `dd`/`dt`, cell and row closing, and "special" elements stopping unmatched end tags;
  - the table, list-item and button scopes;
  - a single-pass adoption agency with reconstruction of formatting elements;
  - table-context clearing, `form` pointer handling and html5ever's newer `select` parsing;
  - breakout from foreign content, and the SVG and MathML integration points.

  Where the emulation is unsure, it errs towards hiding.
- **Time.** Linear in the input with a small constant on top of html5ever's tokenizer. Each tag costs O(1) amortised work:
  - every frame carries what the stack below it implies (table mode, whether a `template` or `table` is open, and the nearest scope boundary of each kind), so no lookup walks the stack;
  - open elements are indexed by name, so "is `p` in button scope" and "which `b` does `</b>` close" are a map lookup and a comparison;
  - each tag name is classified once per scan, never by string matching per token;
  - formatting elements waiting to be reopened are kept per scope marker and capped at 32, so reopening never rescans an ever-growing list;
  - frames that adoption or reconstruction touch were pushed by earlier tokens, so that work is paid for by the input.

  Release build, best of 3, on a shared M3 Pro with a load average of 8 to 11. The bare html5ever tokenizer alone takes 20–48 ms on these inputs there, and the scan stays within about 1.6 times that:

  | Input | Scan | Tokenizer alone | Before Phase 36 |
  |---|---|---|---|
  | 1.6 MB nested `<div><b>` | 44 ms | 36 ms | about 345 ms (2 MiB: 442 ms) |
  | 2 MiB nested formatting | 55 ms | 48 ms | 243–292 ms |
  | 2 MiB misnested formatting | 57 ms | 35 ms | 66 ms |
  | 2 MiB deep tables | 46 ms | 30 ms | 57 ms |
  | 2 MiB deep lists | 55 ms | 35 ms | 492 ms |
  | 2 MiB many anchors | 28 ms | 24 ms | 31 ms |
  | 2 MiB many attributes | 46 ms | 38 ms | 45 ms |
  | 2 MiB long `style` attributes | 5 ms | 1 ms | 22 ms |
  | 2 MiB microdata fan-in (Phase 37) | 41 ms | 35 ms | — |
  | 2 MiB nested microdata items (Phase 37) | 35 ms | 29 ms | — |
  | 2 MiB microdata with many ids (Phase 37) | 45 ms | 34 ms | — |
  | 2 MiB microdata text properties (Phase 37) | 55 ms | 44 ms | — |

  Before Phase 36, every start tag walked the full 256-deep stack and matched tag names as strings, and every `style` attribute was split into owned strings. The tokenizer is most of the time, so on this loaded machine even the bare tokenizer is near or over the 40 ms stage-A target on 2 MiB; an idle run was not measured.
- **Known gap.** A generated tag-soup run over 4 million inputs, each up to 60 pieces of hostile markup, left about 6 inputs per million where a word or href the scan reports is not in the sanitizer's output. All of them mix the adoption agency, active-formatting markers left behind by `</template>`/`</object>`, and foster parenting or foreign content in one input. None of these shapes occur in real mail. The nightly fuzz target finds such cases; each one becomes a fixture and a `VERSION` bump.
- **Not covered:** `<base href>`, relative and protocol-relative hrefs, image `alt` text, CSS from `<style>` blocks or classes (only inline `style` counts), and `mso-hide`.

## Tests

- `src/*`: forward markers, style rules and the sorted dropped-name list.
- `tests/fixtures.rs` over `tests/fixtures/*.html`:
  - one fixture per raw-text element: `rawtext-{style,xmp,iframe,noembed,noframes,noscript}`, `rcdata-{title,textarea}`, `script`, `script-escaped`, `script-ld-json` and `plaintext`;
  - `hidden-display-none-anchor`, `hidden-template-code` and `aria-hidden`;
  - forward, footer, foreign content, dropped elements, emphasis and lines, and malformed markup;
  - microdata: `microdata-lodging` (a `LodgingReservation` with a nested `LodgingBusiness`, `meta content`, `time datetime` and URL values), `microdata-itemref`, `microdata-itemref-cycle`, `microdata-hidden-div` (kept), `microdata-in-template` (nothing from template, SVG or raw text), and `ld-json-below-forward` (`ld_json_forward_at` and an item's `below_forward`); a generated fan-in bomb (1,000 items `itemref`-ing one 1,000-property subtree stops at 4,000 properties) and a generated nesting of 20 items (16 kept).
- `src/microdata.rs`: the `html`/`body` item, the value table, cut text, over-long URLs, misnested markup and inert text.
- `tests/bounds.rs`: 20,000 nested `div`s, a hidden or inert element past the depth cap, the anchor, segment and `ld_json` caps, the 2 MiB char-boundary cut, hostile fragments and determinism. Four microdata bombs (fan-in, nested items, many ids, text properties) join the worst cases; at 2 MiB each keeps at most 4,000 properties and items, item depth 16, every `Item` index inside the list, and sets `truncated_structured`.
  - The worst-case inputs at 64 KiB in every build.
  - Release builds only (`cargo test -p holtmail-html-scan --release`): the 1.6 MB nested `<div><b>` input and each 2 MiB worst case, best of 5 runs, within 40 ms or three times the bare html5ever tokenizer on the same input, whichever is larger. The tokenizer term keeps the test stable on a loaded machine; quadratic work still fails it by a wide margin. CI runs it with `cargo test -p holtmail-html-scan --release --test bounds timing`.
- `tests/differential.rs`: the sanitizer agreement over every fixture, a list of tricky inputs and 20,000 generated tag-soup inputs.
  - It rescans the output of `holtmail_sanitize::sanitize` and checks that every reported `href` is among the rescanned anchors.
  - It checks that every word of every segment occurs in the rescanned text with whitespace removed.
- `fuzz/` (cargo-fuzz, its own workspace, a dated nightly in `fuzz/rust-toolchain.toml`). The target `scan` feeds arbitrary bytes, lossily decoded, to `forward_at` and `scan`, then asserts the same sanitizer agreement, the `ld_json` and microdata caps, `ld_json_forward_at` inside the list, and every `Item` index inside the list. It builds with `cargo fuzz build` on the pinned nightly; when `rustc` on `PATH` is not a rustup proxy (Homebrew Rust), put the toolchain first: `PATH="$(rustup run nightly-2026-10-01 rustc --print sysroot)/bin:$PATH" cargo fuzz build`. Run it with `cd crates/html-scan/fuzz && cargo fuzz run scan`.
