# holtmail-insights

The insights runner. Stage A turns one parsed message into the `message_insights` row: it builds the capped `InsightInput`, runs every extractor inside a panic-safe extraction scope, merges, validates and caps the items, and records header signals and authentication facts. Stage B (trust, kind and topics, `derive_fp`) arrives in Phase 38. It is a plain library: it depends on the contracts (`holtmail-insight`, `holtmail-store`), `holtmail-mime`, `holtmail-auth-results`, `holtmail-html-scan` and `holtmail-people`, and never on an extractor implementation (its tests use the code and unsubscribe extractors as dev-dependencies). `apps/server` builds the extractors and hands them over. The design is [docs/INSIGHTS.md](../../../docs/INSIGHTS.md) §4.4, §6.1 and §6.2.

## Public API

### `Runner`

```rust
Runner::new(extractors: Vec<Arc<dyn InsightExtractor>>) -> Runner
Runner::input(&ParsedMessage) -> (InsightInput, bool)
Runner::extract(&self, &ParsedMessage) -> Extracted
Runner::extract_input(&self, &InsightInput, truncated: bool) -> Extracted
Runner::extract_fp(&self) -> String
Runner::explain(&self, &ParsedMessage) -> String
```

- **Order.** Extractors run in the order given. The composition root passes them as §6.1 lists them: unsubscribe, schemaorg, pass, parcel, links, code. Items keep extractor order, then each extractor's own order.
- **Deterministic.** No clock, randomness, locale or hash-map order reaches the result. Equal input gives byte-equal items and auth JSON.
- **Isolated.** Each `extract` call runs inside an extraction scope and `catch_unwind`. A panic drops that extractor's items and makes the outcome `Partial`. It never reaches the caller.
- **Bounded.** Each extractor contributes at most 16 items, and a row holds at most `MAX_ITEMS` (32) items and `MAX_ITEMS_JSON_BYTES` (16 KiB) of items JSON.
- `extract` is `input` followed by `extract_input`. `extract_input` exists so tests and fixtures can run the pipeline on a hand-built input.
- `explain` is the report behind `holtmail insights explain` (INSIGHTS §12). It runs the same stage A and prints the fingerprint, the input's sizes, the forward position and the `truncated` flag, the header signal names, every extractor's `explain` steps under `[id version]` (run inside the extraction scope, so a panic there is reported as `explain panicked` and the hook keeps its payload out of the logs), and each kept item by kind, confidence, provenance, score, origin part and the skeleton of its value. It never prints a value, host, URL, address, auth fact or text from the message.

`Runner::input` builds what extractors see:

| Part | Rule |
|---|---|
| `headers`, `from`, `reply_to`, `subject` | copied from the parse |
| `sent_at` | `Date` as a `Timestamp` |
| `text` | NFKC of the plain text body, at most 256 KiB of output. Only the first 1 MiB of the original is read, so work stays bounded however long the body is |
| `forward_at` | `holtmail_html_scan::forward_at` over the normalised text |
| `html` | `holtmail_html_scan::scan` over the first 2 MiB of the HTML body, cut on a char boundary |
| `attachments` | every attachment's ordinal, file name, lowercase type and size. Content only for pkpass files (`application/vnd.apple.pkpass` or a `.pkpass` name), at most 2 MiB each and at most 8 in all |

The returned flag is `true` when any of these caps cut something, when the scan returned a list at its own cap (anchors, segments, JSON-LD blocks), or when a message has more attachments than a `u16` ordinal can name.

### `Extracted`

Everything the `InsightRow` stores except the ids, the fingerprint and the time. `Extracted::into_row(message, archive, fingerprint, computed_at)` builds the row.

| Field | Meaning |
|---|---|
| `outcome` | `Ok`, or `Partial` when an extractor panicked. The runner never produces `Unparsable` or `Unreadable`; the walk and reindex write those |
| `truncated` | an input cap was hit, or the row cap dropped items |
| `from_address` | the first `From` address, lowercase, its domain as an A-label. `None` when the domain does not parse as a host name |
| `signals` | the header bits of INSIGHTS §4.4, plus `structured` |
| `kinds` | `KindTag` bits of the kept items at `Likely` or higher that are not forwarded |
| `auth` | the auth facts JSON below |
| `items` | the kept items; `items.to_json()` is the stored column |

### `Signals`

| Bit | Set when |
|---|---|
| `list_id` | a `List-Id` field exists |
| `list_post` | a `List-Post` other than `NO` exists |
| `list_post_no` | a `List-Post` that is `NO` after comments are stripped, letter case ignored |
| `unsubscribe` | a `List-Unsubscribe` field exists |
| `unsubscribe_post` | a `List-Unsubscribe-Post` field exists |
| `precedence_bulk` | `Precedence` is `bulk`, `list` or `junk` |
| `auto_submitted` | `Auto-Submitted` is neither `no` nor `auto-replied` |
| `auto_replied` | `Auto-Submitted: auto-replied` |
| `feedback_id` | a `Feedback-ID` field exists |
| `esp` | `X-SG-EID`, `X-MC-User`, `X-SES-Outgoing`, `X-PM-Message-Id` or any `X-Mailgun-*` field exists |
| `noreply` | the first `From` local part passes `holtmail_people::is_automated_local` |
| `forward_marker` | the text has an inline-forward marker, or the scanned HTML has content below one |
| `structured` | a kept, non-forwarded `Certain` item comes from JSON-LD, microdata or a pkpass |

### Auth facts

```json
{"v":1,
 "results":[{"pos":3,"authserv":"mx.example.org","dkim":[{"d":"news.example.com","i":null,"s":"s1","b":"AbC12345"}]}],
 "signatures":[{"d":"news.example.com","s":"s1","b":"AbC12345","h":{"from":1,"list-unsubscribe":2},"l":false}],
 "fields":{"from":1,"list-unsubscribe":1,"list-unsubscribe-post":0},
 "sig_cap_hit":false,
 "from_domain":"news.example.com"}
```

- `results`: the first 8 readable `Authentication-Results` fields, topmost first. `pos` is the field's index among all header fields. `dkim` lists only the `dkim=pass` results of that field, at most 8, with `b` cut to 8 characters.
- `signatures`: readable `DKIM-Signature` fields in header order. A signature whose `d` equals the `header.d` or `header.i` domain of a recorded pass is kept first, up to 8. Others fill the remaining room. `sig_cap_hit` is set when a matching signature had to be dropped.
- `h` counts how often `from`, `list-unsubscribe` and `list-unsubscribe-post` are signed; names signed zero times are left out. `l` says an `l=` tag is present.
- `fields` counts the `From`, `List-Unsubscribe` and `List-Unsubscribe-Post` fields.
- Names longer than 255 bytes (`d`, `i`, `s`, an authserv-id) are left out (`null`, or the whole result for an authserv-id), so a forged field cannot bloat the row.

### Merge, validate, cap

1. **Normalise.** A no-op until `holtmail-links` lands in Phase 36.
2. **Merge.** Identical items are kept once. A code equal to a reservation, order, invoice or parcel reference (letters and digits, letter case ignored) is dropped. A heuristic action whose URL equals a structured action's URL is dropped. Grouping code candidates by value is the code extractor's job.
3. **Validate.** Items are dropped when an identifier does not fit `fits_identifier`, an action has a `mailto` URL or an over-long destination, an unsubscribe item has no link, a link of the wrong scheme, a `mailto` that is not a `mailto:` URL or `one_click` without `https`, an order lacks both number and merchant, an invoice lacks both total and due date, a reservation lacks its required fields or ends before it starts (both times local or both zoned), or an item names an attachment the message does not have. Display text is cut with `truncate_display`. Scores are clamped to 1000, and `Certain` scores 1000.
4. **Cap.** Items are ranked: higher confidence, then structured provenance before heuristic, then not forwarded before forwarded, then earlier reservation legs before later ones, then higher score, then earlier position. They are taken in that order while the row stays within 32 items and 16 KiB of JSON. An item too large for the remaining room is skipped, and smaller lower-ranked items may still fit. The kept items keep their original order.

### `extract_fp`

The hex BLAKE3 hash of these lines:

```
insights=<VERSION>
html-scan=<holtmail_html_scan::VERSION>
auth-results=<holtmail_auth_results::VERSION>
<id>=<version>      one per extractor, in run order
```

Each extractor's version is read once, in `Runner::new`. `links::VERSION` joins in Phase 36.

### Panic hook

- `install_panic_hook()`: `apps/server` calls it at start. It chains the previous hook and installs only once.
- Inside an extraction scope it logs one `tracing` error naming the extractor id, version, message id and `file:line`. It never logs the payload, which can quote message content (an `assert!` or `expect` message that formats a value).
- Outside a scope it defers to the previous hook.
- `with_message(id, f)` sets the message id the report names for the duration of `f`, on this thread.

### `NormalizedText`

`NormalizedText::new(original, max_bytes)` is the NFKC step `input` uses. `original_offset(byte)` maps a byte offset in the normalised text back to the start of the original run it came from. The text is cut only before a character that is a stable NFKC boundary (combining class 0 and quick check Yes), or after 32 characters with no such boundary. So the kept text normalises exactly as the whole text would, except inside pathological runs of more than 32 combining characters.

## Unwinding

Extraction depends on panics unwinding. The workspace sets no `panic = "abort"`:

- `no_profile_aborts_on_panic` reads the root `Cargo.toml` and fails if any profile sets it. It is part of `cargo test`, so `mise run check` enforces it.
- `panics_unwind` also runs with `--release` in CI (`cargo test -p holtmail-insights --release panics_unwind`).

A stack overflow aborts regardless, which is why every input is capped and nothing recurses over input.

## Tests

`cargo test -p holtmail-insights` covers:

- determinism;
- the input caps (text, HTML, pkpass content);
- the per-extractor and row caps with the drop order;
- merge and validation;
- kinds and signals;
- panic isolation;
- the panic-report leak test, where a panic message quotes a secret from the input and the captured report must not;
- `panics_unwind` and the profile check;
- the fingerprint;
- the shared hostile corpus of `holtmail_insight::testing`, run twice through the runner;
- `tests/fixtures.rs`: `smtp-forged-ar-no-trust.eml`, whose forged `Authentication-Results` is recorded as written (trust is stage B's decision).

- `tests/corpus.rs`: the precision gate below.
- `tests/golden.rs`: the golden snapshot below.
- `tests/explain.rs`: for every corpus message, the `explain` report contains no host, no `@`, no labelled code and no labelled unsubscribe link; and it names the expected rules for an origin-bound code, a code next to an order number and a one-click newsletter.
- `tests/bounds.rs`: a test-only counting `GlobalAlloc` (the only `unsafe` in the crate; the library keeps `#![forbid(unsafe_code)]`, and the crate's lint table denies rather than forbids `unsafe_code` for that reason) checks that no single allocation exceeds 32 MiB while the runner, with the code and unsubscribe extractors, handles the shared hostile corpus and six hostile messages (8 MiB of one token, 200,000 code lines, 40,000 nested tags, 20,000 anchors, a million combining marks, 4,000 header fields). In release builds it also asserts at most 40 ms per case for `extract_input`; building the input (MIME text, NFKC, the HTML scan) is timed and printed only. In debug builds it just has to finish.

## Corpus gate

`tests/corpus/` holds invented `.eml` messages (codes in 12 languages, hard negatives, unsubscribe header variants) and `labels.toml`, one `[[case]]` per file:

```toml
[[case]]
file = "otp-plain-en.eml"
code = "482913"                 # the code the card must show; absent means none
origin = "example.com"          # only for a WICG origin-bound code
negative = "order_number"       # the hard-negative family of a message without a code
unsubscribe = { https = "https://news.example.com/u/1", mailto = "mailto:leave@news.example.com", one_click = true, list_id = "weekly.news.example.com" }
```

`cargo test -p holtmail-insights --test corpus` runs the runner with `UnsubscribeExtractor` and `CodeExtractor` (in the server's order) over every file through `holtmail_mime::parse`, twice, and requires equal results. A code counts as found when it is `Likely` or higher and not forwarded, as the card and `has:code` see it; `Possible` codes are listed separately. An unsubscribe item must equal its label field for field. The test prints a confusion summary with case names and fails below codes precision 0.98 or recall 0.85, or unsubscribe precision or recall 1.0. It also requires at least 150 cases, at least 50 of them labelled `negative`, and exactly one label per file.

When the gate fails, fix the extractor's rules or the weights in its data file, not the labels, unless a label is wrong. Action links, parcels, kinds and topics join the labels in Phases 36 to 38.

## Golden snapshot

`tests/golden.rs` renders the stage A row of every corpus message (outcome, `truncated`, from address, signal and kind bits, auth facts JSON, items JSON) under the current `extract_fp` and compares it with `tests/golden.txt`. Any drift fails, including drift from a dependency update (html5ever, regex, serde_json, url and so on):

- When the rows changed but `extract_fp` did not, stored rows would never be recomputed: bump the `VERSION` of what changed (`holtmail_insights::VERSION`, `holtmail_html_scan::VERSION`, `holtmail_auth_results::VERSION`, or the extractor's version), then rewrite the snapshot.
- When `extract_fp` changed as intended, review the diff and rewrite the snapshot.

Rewrite it with `UPDATE_GOLDEN=1 cargo test -p holtmail-insights --test golden`.

## Fuzzing

`fuzz/` is a `cargo-fuzz` crate with its own empty `[workspace]` and a `rust-toolchain.toml` pinning a dated nightly. Its `extract` target parses arbitrary bytes with `holtmail_mime::parse` and runs the runner with the code and unsubscribe extractors twice, checking equal results and the row caps, then `explain`:

```sh
cd crates/insights/fuzz && cargo fuzz run extract
```
