# holtmail-insight-unsubscribe

An `InsightExtractor` (id `unsubscribe`) that reads the unsubscribe options a message declares in its header fields: the first `https`, `http` and `mailto` link of `List-Unsubscribe` (RFC 2369), whether RFC 8058 one-click is declared, and the `List-Id` (RFC 2919). It reads header fields only and makes no request. Whether a one-click request is covered by a trusted DKIM signature (`one_click_ok`) is decided later, in stage B of the runner, from the auth facts; this crate does not compute it. The design is [docs/INSIGHTS.md §7.2](../../../docs/INSIGHTS.md).

## API

```rust
pub const ID: &str = "unsubscribe";
pub const VERSION: &str = "1";
pub const LIST_UNSUBSCRIBE: &str = "List-Unsubscribe";
pub const LIST_UNSUBSCRIBE_POST: &str = "List-Unsubscribe-Post";
pub const LIST_ID: &str = "List-Id";

#[derive(Debug, Clone, Copy, Default)]
pub struct UnsubscribeExtractor;          // UnsubscribeExtractor::new(); impl InsightExtractor

pub struct UnsubscribeLinks { pub https: Option<Url>, pub http: Option<Url>, pub mailto: Option<Url> }
impl UnsubscribeLinks { pub fn is_empty(&self) -> bool }

pub fn parse_list_unsubscribe(value: &str) -> UnsubscribeLinks;
pub fn is_one_click_post(value: &str) -> bool;
pub fn parse_list_id(value: &str) -> Option<String>;
```

### `UnsubscribeExtractor`

On top of the `InsightExtractor` contract (deterministic, no I/O, never panics, nothing on personal mail):

- **At most one item**, an `InsightKind::Unsubscribe` with `Provenance::Header`, `Confidence::Certain`, score 1000, origin `PartRef::Header("List-Unsubscribe")` and `forwarded` false.
- **An item only when there is a link.** The topmost `List-Unsubscribe` field is read with `parse_list_unsubscribe`; when it gives no link there is no item, whatever `List-Unsubscribe-Post` or `List-Id` say.
- **Multiple fields.** When the message has more than one `List-Unsubscribe` field, only the topmost is read for links, and `one_click` is false.
- **`one_click`** is true only with exactly one `List-Unsubscribe` field, exactly one `List-Unsubscribe-Post` field whose value passes `is_one_click_post`, and an `https` link.
- **`mailto`** is the normalised `mailto:` URI as a string, as the contract's `Unsubscribe.mailto` field requires.
- **`list_id`** comes from the topmost `List-Id` field through `parse_list_id`; a later `List-Id` is never read.
- Field names match in any letter case. The body, HTML and attachments never change the output.
- **`explain`** lists the `List-Unsubscribe` field count, each item of the topmost field by position (`item:https`, `item:http`, `item:mailto`, `item:later_of_same_scheme`, `item:refused`, `item:unbracketed_stop`, `item:unclosed_stop`), the `List-Unsubscribe-Post` check with its field count, each reason one-click is withheld (`one_click:not_one_list_unsubscribe`, `one_click:not_one_post`, `one_click:post_value`, `one_click:no_https`), whether `List-Id` is valid, and the outcome. No link, address or list id appears in it.

### `parse_list_unsubscribe`

RFC 2369, strictly:

- Items are comma-separated and read left to right. Only `<…>` items count; whitespace inside the brackets is removed. After a `>`, everything up to the next comma is ignored (a comment, for example).
- Reading stops at the first item that does not start with `<` or has no closing `>`. A link after an unbracketed item is never taken.
- Only links that are a valid `holtmail_insight::Url` with scheme `https`, `http` or `mailto` are kept, the first of each scheme. `file:`, `javascript:`, relative, malformed, userinfo-carrying and over-long links are skipped, and reading continues.
- Never panics; linear in the length of the value.

### `is_one_click_post`

True only when the value, trimmed, is exactly `List-Unsubscribe=One-Click`, letter case included.

### `parse_list_id`

The content of the last `<…>` in a `List-Id` value, lowercased (list identifiers are case-insensitive). `None` unless that content is a dot-atom (`atext` labels joined by single dots) of at most 255 bytes with at least one dot. A bare identifier without brackets is refused.

## Limits

- Header fields only; no coverage, alignment or trust decision is made here (stage B, Phase 38).
- No POST and no network access; the one-click request belongs to the server (Phase 41).
- A comment containing a comma after an item ends reading at the next item, as the strict grammar requires.
- An `https` link the contract's `Url` refuses (userinfo, over 8 KiB) is skipped, so a later `https` link may be taken in its place.

## Tests

- `src/parse.rs`: unit tests for the grammar, one-click value and `List-Id`.
- `tests/conformance.rs`: every unsubscribe fixture of INSIGHTS §15 (`unsub-one-click-covered`, `unsub-one-click-h-missing-post`, `unsub-one-click-other-selector`, `unsub-one-click-unaligned`, `unsub-prepended-second-field`, `unsub-not-oversigned`, `unsub-bad-item-stops`, `unsub-file-scheme`) rebuilt as invented `InsightInput` header sets, plus one case per rule. Each case checks the exact output, and all of them run through `holtmail_insight::testing::conformance::extractor`. The coverage fixtures differ only in their DKIM and `Authentication-Results` fields, so here they all give `one_click: true` except the prepended second field; stage B tells them apart.
- `tests/fixtures.rs`: the same eight fixtures as invented `.eml` files in `tests/fixtures/`, read through `holtmail_mime::parse` and `holtmail_insights::Runner::input`. It checks the exact item, that the auth facts the runner records carry what stage B needs to tell the coverage fixtures apart (signed header counts, selectors, signing domains, field counts), that `explain` lists the expected steps without any link, and runs the conformance suite over them.
- `tests/properties.rs`: proptest that `parse_list_unsubscribe` and `parse_list_id` never panic and that links only ever have their slot's scheme.

```sh
cargo test -p holtmail-insight-unsubscribe
```
