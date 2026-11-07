# holtmail-insight

The contract for message insight extractors. An `InsightExtractor` reads one message, given as an `InsightInput`, and returns `Insight` items: one-time codes, action links, the unsubscribe options, reservations, orders, invoices, parcels and passes. The crate also holds the persisted bit sets (`Signals`, `KindTags`) and the stored JSON shape of a row's items (`Items`). It has no parser, no I/O and no merge logic: input building, merging, validation and caps live in the runner, `holtmail-insights`. The design is [docs/INSIGHTS.md](../../../docs/INSIGHTS.md).

## API

### The extractor

```rust
pub trait InsightExtractor: Send + Sync {
    fn id(&self) -> &str;
    fn version(&self) -> String;
    fn extract(&self, input: &InsightInput) -> Vec<Insight>;
    fn explain(&self, input: &InsightInput) -> Vec<Explanation> { Vec::new() }
}
```

Every implementation guarantees:

- **Deterministic.** Equal input gives equal output, in the same order. No clock, randomness, locale or hash-map order reaches the output. The "now" for dates is `input.sent_at`.
- **Pure.** No I/O, no shared mutable state, no dependence on another extractor. Rules across extractors (a code equal to an order number) belong to the runner.
- **Never fails, never panics.** Input it cannot read gives fewer items or none.
- **Quiet on personal mail.** A plain message from one person to another gives nothing.
- **Bounded.** At most 16 items. Identifiers fit `fits_identifier` and are never cut. Display text goes through `truncate_display`. No identifier or URL is a prefix of a longer value in the input.
- `id()` is a short stable name (`code`, `unsubscribe`). `version()` is equal for the life of the process and changes whenever the output for some input may change, including the hash of every embedded data table.
- **`explain` never carries content.** It lists the steps behind `extract` for an operator's terminal (`holtmail insights explain`, INSIGHTS §12) and holds only what an `Explanation` can hold. It is deterministic and never panics. The default returns nothing.

### Explanations

```rust
pub struct Explanation {
    pub rule: &'static str,        // a name fixed in code: "money", "outcome:tie"
    pub part: Option<PartRef>,
    pub at: Option<usize>,         // char offset in the part, or position in a list
    pub score: Option<i32>,
    pub count: Option<usize>,
    pub skeleton: Option<Skeleton>,
    pub keywords: Vec<String>,     // only words of the extractor's own tables
}
// Explanation::new(rule).part(..).at(..).score(..).count(..).skeleton(..).keyword(..)

pub struct Skeleton;   // Skeleton::of(&str), as_str(), Display
```

- `rule` is `&'static str`, so a rule name cannot carry message text.
- `Skeleton::of` keeps only the shape: every digit becomes `D`, every letter `a`, whitespace a space, ASCII punctuation stays and anything else becomes `*`, at most 64 chars and then `…`. `482-913` becomes `DDD-DDD`.
- Values, hosts, URLs, addresses and free text never go in an explanation.

### Input

- `InsightInput { headers, from, reply_to, subject, sent_at, text, forward_at, html, attachments }`. Headers are topmost first, unfolded and decoded (`HeaderField { name, value }`); `from` and `reply_to` are `holtmail_core::Address`; `sent_at` is a `holtmail_core::Timestamp`. `text` is the plain body, NFKC-normalised, at most 256 KiB, empty when there is none. `forward_at` is the char offset in `text` of the first inline-forward marker. `header_values(name)` gives the values of one field name, any letter case, topmost first.
- `HtmlView` is `#[non_exhaustive]`: build it with `HtmlView::new().with_anchors(..).with_ld_json(..).with_ld_json_forward_at(..).with_microdata(..).with_segments(..).with_footer_at(..).with_truncated_structured(..)`, or set the public fields of `HtmlView::default()`. Hidden content is already removed from anchors and segments; JSON-LD and microdata are kept inside CSS- and `aria`-hidden subtrees, never inside `template`, foreign content or what the sanitizer strips.
  - `anchors: Vec<Anchor>`; `Anchor { href, text, aria_label, button_like, in_footer, below_forward, index }`. `href` is as written and not validated.
  - `ld_json: Vec<String>`: the raw `application/ld+json` blocks, in document order.
  - `ld_json_forward_at: Option<usize>`: the index in `ld_json` of the first block below an inline-forward marker; every block from it on is below the marker. `None` when no block is. It may be out of range in a hand-built view, so readers compare, never index.
  - `microdata: Vec<MicrodataItem>`: flat; a nested item is `MicrodataValue::Item(index)` into this list. Indexes may form cycles or point outside the list, so readers bound their walk. `MicrodataItem` is `#[non_exhaustive]` (`MicrodataItem::new(types)`) with `types`, `id`, `properties`, `top_level` and `below_forward` (the `itemscope` element sits below an inline-forward marker).
  - `segments: Vec<Segment>`; `Segment { text, emphasis, alone_on_line, below_forward }`.
  - `footer_at`: the index in `segments` of the first segment at or after the footer marker.
  - `truncated_structured: bool`: a JSON-LD or microdata cap left something out (a block dropped for size or count, items or properties past the depth, item or property caps). `false` means `ld_json` and `microdata` hold everything the scanner read; the list lengths alone do not tell. The runner ORs it into the row's `truncated`.
- `AttachmentMeta { ordinal, filename, content_type, size, content }`. `content` is present only for pkpass attachments.
- `limits` holds every cap of the contract: 16 items per extractor, 64-char identifiers, 512-char display text, 8 KiB URLs, 2 KiB destinations, and the input caps (256 KiB text, 2 MiB HTML, 500 anchors, 8 JSON-LD blocks of 256 KiB, microdata depth 16 and 4,000 properties, 4,000 segments, pkpass 2 MiB each and 8 in all). The runner applies the input caps; extractors must still stay correct above them.

### Items

- `Insight { kind, provenance, confidence, score, origin, forwarded }`. `score` is `0..=1000` and `1000` for `Certain`. `forwarded` items never set the message's kind or topics.
- `InsightKind`: `Code { value, origin_bound }`, `Action { action, url, destination, via, tracked, text_host, label }`, `Unsubscribe { https, mailto, http, one_click, list_id }`, `Reservation(Reservation)`, `Order { number, merchant, total, status, url }`, `Invoice { number, payee, total, due, url }`, `Parcel { carrier, number, check_digit, track_url }`, `Pass { style, organization, description, relevant, boarding, attachment, signature_checked }`. `kind.tag()` gives its `KindTag`.
- `Reservation`: `Flight`, `Lodging`, `Train`, `Bus`, `Event`, `Food`, `RentalCar`, each with its own struct (`FlightReservation`, ...). Plain data.
- `Provenance`: `Header`, `JsonLd`, `Microdata`, `PkPass`, `OriginBound`, `Heuristic`. `Confidence`: `Possible < Likely < Certain`. `PartRef`: `Header(name)`, `Subject`, `Text`, `Html`, `Attachment(ordinal)`. `LinkAction`: `verify`, `confirm`, `activate`, `reset_password`, `sign_in`, `magic_link`, `view_order`, `track`, `check_in`, `wallet_save`, `view`.
- `Url`: `https`, `http` or `mailto` only; `http(s)` has a host, no userinfo, and an A-label host; `mailto` has an address and no authority; at most 8 KiB before and after normalisation, rejected and never cut. `fits_destination()` checks the 2 KiB destination limit. Reading one from JSON applies the same rules.
- `Money { minor: i64, currency }`, never a float. `Currency::new(code, exponent)` takes three uppercase ASCII letters and an exponent up to 4; which codes exist is the extractor's data.
- `Date(NaiveDate)`. `LocalOrZoned::Local(NaiveDateTime)` or `Zoned(DateTime<FixedOffset>, Option<zone name>)`: a time without an offset is never shifted.
- `CarrierId::new(id)`: 1–32 chars of `[a-z0-9_]`. `PassStyle`: `boarding_pass`, `event_ticket`, `coupon`, `store_card`, `generic`. `BoardingPass { pnr, passenger, legs }`, `BoardingLeg { from, to, carrier, flight, date, seat }`, `PassDate::Full(Date)` or `MonthDay { month, day }`.
- `fits_identifier(value)`: 1 to 64 chars. `truncate_display(text)`: at most 512 chars, cut on a char boundary with `…`.
- `InsightError`: `Unparsable` or `Decode(rule)`. Its message names the rule, never the content.

### Persisted bits

Bit positions are append-only and never reused.

| Bit | `Signal` | `KindTag` |
|---|---|---|
| 0 | `list_id` | `code` |
| 1 | `list_post` | `link` |
| 2 | `list_post_no` | `unsubscribe` |
| 3 | `unsubscribe` | `reservation` |
| 4 | `unsubscribe_post` | `order` |
| 5 | `precedence_bulk` | `invoice` |
| 6 | `auto_submitted` | `parcel` |
| 7 | `auto_replied` | `pass` |
| 8 | `feedback_id` | |
| 9 | `esp` | |
| 10 | `noreply` | |
| 11 | `forward_marker` | |
| 12 | `structured` | |

`Signals` and `KindTags` are `u64` sets (`from_bits`, `bits`, `contains`, `insert`, `remove`, `with`, `iter`, `FromIterator`, `|`). Unknown bits are kept, so a value written by a newer build survives a round trip. `Topic` bits belong to `holtmail-category`.

### Items JSON

- `Items::to_json()` writes `{"v":1,"items":[...]}`. Each item is one object: the `InsightKind` fields tagged by `"type"` (snake_case), plus `p` (provenance), `c` (confidence), `score`, `origin` and `forwarded`. Reservations nest as `{"type":"reservation","r":{"type":"flight",...}}`. Equal items give equal bytes.
- `Items::from_json(json) -> Result<ItemsRead, InsightError>` is lenient: an item with an unknown type or bad fields is skipped and counted in `skipped`, the others kept in order. Only text that is not an object with an integer `v` and an `items` array fails, with `Unparsable`. `ItemsRead::outdated()` is true when something was skipped or `v` is not `ITEMS_VERSION`, and the row is then served as `outdated` and recomputed.

## Implementing an extractor

1. Create `crates/insight-<name>` depending on `holtmail-insight`.
2. Build values with the contract's checks: `Url::parse`, `fits_identifier`, `truncate_display`, `Currency::new`, `CarrierId::new`.
3. Add `holtmail-insight = { workspace = true, features = ["testing"] }` under `[dev-dependencies]` and run the suite with your fixture cases:

```rust
use holtmail_insight::testing::{Case, Expect, conformance};

#[test]
fn keeps_the_contract() {
    conformance::extractor(&MyExtractor::new(), &[Case::new("otp-plain-en", input, Expect::Found)]);
}
```

`conformance::extractor(&dyn InsightExtractor, &[Case])` panics on the first violation. On top of your cases it runs `testing::personal_mail()` (which must give nothing) and `testing::hostile_corpus()`, and checks for each input: a stable `id()` and `version()`; no panic (`catch_unwind`); byte-equal items JSON from two runs; at most 16 items; scores in range and `1000` for `Certain`; a lossless JSON round trip; identifier and display-text limits; URL and destination limits; `mailto` strings that parse as `mailto:` URLs; `https`/`http` unsubscribe slots of their own scheme and `one_click` only with `https`; attachment references that exist; no identifier or URL that is only a prefix of a longer input value; each case's `Expect` (`Nothing`, `Found`, `Any`); and that `explain` does not panic, uses rule names of lowercase letters, digits, `_`, `:` and `-`, keeps skeletons free of letters and digits other than `D` and `a`, keeps keywords within 64 chars, and never contains a value, URL, host, `mailto` address or list id the extractor returned for that input.

The hostile corpus holds 10 MB of one token (digits in the text, alphanumerics in one segment), 10 MB header values and 100,000 header fields, 1,000 anchors with refused schemes, userinfo, IDN hosts and over-long URLs, 5,000 segments, text with replacement characters where invalid UTF-8 was, multi-byte text at every slice offset, out-of-range positions and extreme `sent_at`, JSON-LD at depth 500 with an `@id` cycle and a huge block, microdata cycles and an `itemref` fan-in bomb, a view whose `ld_json_forward_at` and `Item` index point past their lists, and pkpass attachments with a deflate bomb (16 MiB from about 100 KiB, honest and lying sizes), `..` and absolute entry names, and broken zips.
