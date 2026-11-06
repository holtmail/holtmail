# holtmail-html-scan

Turns a message's HTML into the `HtmlView` that insight extractors read (visible anchors, visible text segments with emphasis and line flags, raw `ld+json` blocks and the footer marker), and finds the inline-forward marker in a plain-text body. A plain library on the html5ever **tokenizer**: no DOM is built, so memory stays linear in the input and nothing recurses over it. The tree-building rules that decide what ends up inside what are emulated with an explicit element stack. `holtmail-insights` calls it once per message while building `InsightInput`. The design is in [INSIGHTS.md](../../../docs/INSIGHTS.md) §7.0 and §6.1.

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
  - At most `limits::MAX_LD_JSON_BLOCKS` (8) are kept. A block larger than `limits::MAX_LD_JSON_BYTES` (256 KiB) is dropped whole, never cut.
- **`microdata`** stays empty until Phase 37.
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

`"1"`. It changes whenever the same input could give a different view or offset, and it is part of `extract_fp`.

## Limits and deliberate choices

- **Depth.** The element stack holds 256 elements. Deeper elements are flat: they are not pushed and do not change emphasis or button context, and anchors that open past the cap are not reported. A hidden element past the cap hides everything until the stack shrinks again. An inert one (`template`, foreign content, a dropped name) hides everything to the end of the input, so a hidden subtree is never unhidden early.
- **Tree-builder emulation.** The scan does not build a DOM. It emulates the rules that decide nesting:
  - implied `p`, `li`, `dd`/`dt`, cell and row closing, and "special" elements stopping unmatched end tags;
  - the table, list-item and button scopes;
  - a single-pass adoption agency with reconstruction of formatting elements;
  - table-context clearing, `form` pointer handling and html5ever's newer `select` parsing;
  - breakout from foreign content, and the SVG and MathML integration points.

  Where the emulation is unsure, it errs towards hiding.
- **Known gap.** A generated tag-soup run over 4 million inputs, each up to 60 pieces of hostile markup, left about 6 inputs per million where a word or href the scan reports is not in the sanitizer's output. All of them mix the adoption agency, active-formatting markers left behind by `</template>`/`</object>`, and foster parenting or foreign content in one input. None of these shapes occur in real mail. The nightly fuzz target finds such cases; each one becomes a fixture and a `VERSION` bump.
- **Not covered:** `<base href>`, relative and protocol-relative hrefs, image `alt` text, CSS from `<style>` blocks or classes (only inline `style` counts), and `mso-hide`.

## Tests

- `src/*`: forward markers, style rules and the sorted dropped-name list.
- `tests/fixtures.rs` over `tests/fixtures/*.html`:
  - one fixture per raw-text element: `rawtext-{style,xmp,iframe,noembed,noframes,noscript}`, `rcdata-{title,textarea}`, `script`, `script-escaped`, `script-ld-json` and `plaintext`;
  - `hidden-display-none-anchor`, `hidden-template-code` and `aria-hidden`;
  - forward, footer, foreign content, dropped elements, emphasis and lines, and malformed markup.
- `tests/bounds.rs`: 20,000 nested `div`s, a hidden or inert element past the depth cap, the anchor, segment and `ld_json` caps, the 2 MiB char-boundary cut, hostile fragments and determinism.
- `tests/differential.rs`: the sanitizer agreement over every fixture, a list of tricky inputs and 20,000 generated tag-soup inputs.
  - It rescans the output of `holtmail_sanitize::sanitize` and checks that every reported `href` is among the rescanned anchors.
  - It checks that every word of every segment occurs in the rescanned text with whitespace removed.
- `fuzz/` (cargo-fuzz, its own workspace, a dated nightly in `fuzz/rust-toolchain.toml`). The target `scan` feeds arbitrary bytes, lossily decoded, to `forward_at` and `scan`, then asserts the same sanitizer agreement. Run it with `cd crates/html-scan/fuzz && cargo fuzz run scan`.
