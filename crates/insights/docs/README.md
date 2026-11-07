# holtmail-insights

The insights runner. Stage A turns one parsed message into the `message_insights` row: it builds the capped `InsightInput`, runs every extractor inside a panic-safe extraction scope, merges, validates and caps the items, and records header signals and authentication facts. Stage B (`Deriver`) turns the stored stage-A columns, the stored alias row, the trust lists and the archive's own addresses into trust, one-click coverage, sender, kind and topics, with no blob read. It is a plain library: it depends on the contracts (`holtmail-insight`, `holtmail-category`, `holtmail-store`), `holtmail-mime`, `holtmail-auth-results`, `holtmail-html-scan`, `holtmail-links` and `holtmail-people`, and never on an extractor or categorizer implementation (its tests use the unsubscribe, schemaorg, pass, parcel, links and code extractors, `category-signals` and `store-sqlite` as dev-dependencies). `apps/server` builds the extractors and categorizers and hands them over. The design is [docs/INSIGHTS.md](../../../docs/INSIGHTS.md) §4.2 to §4.5, §6.1, §6.2, §6.5 and §7.2.

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
- `explain` is the report behind `holtmail insights explain` (INSIGHTS §12). It runs the same stage A and prints the fingerprint, the input's sizes, the forward position and the `truncated` flag, the header signal names, every extractor's `explain` steps under `[id version]` (run inside the extraction scope, so a panic there is reported as `explain panicked` and the hook keeps its payload out of the logs), and each kept item by kind, confidence, provenance, score, origin part and the skeleton of its value. An action adds its `action` name and whether `via`, `tracked`, `text_host` and a label are set; a parcel adds its carrier id, the skeleton of its number, `check_digit` and whether it has a tracking URL; a reservation adds its type, the skeleton of its reservation number, whether its start is local or zoned, and its passenger (or guest) and seat (or ticket) counts; an order or invoice adds the skeleton of its number and which fields are set; a pass adds its style, attachment ordinal, whether its date is local or zoned and its boarding leg count. It never prints a value, host, URL, address, auth fact or text from the message.

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
{"v":2,
 "results":[{"pos":3,"authserv":"mx.example.org","dkim":[{"d":"news.example.com","i":null,"s":"s1","b":"AbC12345"}]}],
 "signatures":[{"d":"news.example.com","s":"s1","b":"AbC12345","h":{"from":1,"list-unsubscribe":2},"l":false}],
 "fields":{"from":1,"list-unsubscribe":1,"list-unsubscribe-post":0},
 "sig_cap_hit":false,
 "from_domain":"news.example.com",
 "from_addresses":1}
```

- `results`: the first 8 readable `Authentication-Results` fields, topmost first. `pos` is the field's index among all header fields. `dkim` lists only the `dkim=pass` results of that field, at most 8, with `b` cut to 8 characters.
- `signatures`: readable `DKIM-Signature` fields in header order. A signature whose `d` equals the `header.d` or `header.i` domain of a recorded pass is kept first, up to 8. Others fill the remaining room. `sig_cap_hit` is set when a matching signature had to be dropped.
- `h` counts how often `from`, `list-unsubscribe` and `list-unsubscribe-post` are signed; names signed zero times are left out. `l` says an `l=` tag is present.
- `fields` counts the `From`, `List-Unsubscribe` and `List-Unsubscribe-Post` fields. `from_addresses` counts the addresses of the parsed `From`.
- Names longer than 255 bytes (`d`, `i`, `s`, an authserv-id) are left out (`null`, or the whole result for an authserv-id), so a forged field cannot bloat the row.

### Merge, validate, cap

1. **Normalise.** Every action's `url` goes through `holtmail_links::unwrap`. `destination` becomes the unwrapped URL (or `url` itself when nothing was unwrapped), `via` the wrapper's registrable domain (`google.com`, `outlook.com`, `proofpoint.com`) or `None`, and `tracked` whether `holtmail_links::tracker` knows the destination's host and path. An action whose destination does not parse as a contract `Url` or is longer than `MAX_DESTINATION_BYTES` (2 KiB) is dropped. `url` and every other field stay as the extractor wrote them. IATA codes are trimmed and uppercased: a flight reservation whose departure or arrival airport is not three ASCII letters is dropped, a boarding leg of a pass with such an airport is removed from its pass, and a carrier that is not two ASCII letters or digits becomes `None`. Currencies need no step: the contract's `Currency` only holds an uppercase ISO code with its exponent. Other kinds pass unchanged.
2. **Merge.** Identical items are kept once. Items of one fact are then joined by their merge key (INSIGHTS §7.5), from any extractor:

   | Kind | Merge key |
   |---|---|
   | Flight | carrier + flight number + local departure date; without a flight number, reservation number + departure airport + local departure date |
   | Lodging | reservation number + check-in date |
   | Train, Bus | train or bus number (else reservation number) + departure station + local departure date |
   | Event, Food | name + start (a zoned start as its instant, a local one as written) |
   | RentalCar | reservation number + pickup date |
   | Order | number, else merchant |
   | Invoice | number, else payee; neither gives no key |
   | Parcel | carrier + number |
   | Pass | attachment ordinal |

   Numbers compare by their letters and digits, letter case ignored; names and stations with their whitespace collapsed, letter case ignored; dates are the local date in the time's own offset. The item kept has the higher provenance (JSON-LD, then microdata, then pkpass, then heuristic), then the higher confidence, then the higher score, then the earlier position, and it takes the place of the first item of its key. It is `forwarded` only when every joined item was. A kept parcel also takes `check_digit` and a missing `track_url` from the parcels it absorbed, so a JSON-LD parcel keeps the heuristic check and carrier link. A code equal to a reservation, order, invoice or parcel reference (letters and digits, letter case ignored) is dropped. A heuristic action is dropped when a structured action (JSON-LD, microdata or pkpass) has the same `LinkAction` or the same destination. A forwarded reference or structured action drops only forwarded codes and actions, never the sender's own. Grouping code candidates by value is the code extractor's job.
3. **Validate.** Items are dropped when an identifier does not fit `fits_identifier`, an action has a `mailto` URL or an over-long destination, an unsubscribe item has no link, a link of the wrong scheme, a `mailto` that is not a `mailto:` URL or `one_click` without `https`, an order lacks both number and merchant, an invoice lacks both total and due date, a reservation lacks its required fields or ends before it starts (both times local or both zoned), or an item names an attachment the message does not have. Display text is cut with `truncate_display`. Scores are clamped to 1000, and `Certain` scores 1000.
4. **Cap.** Items are ranked: higher confidence, then structured provenance before heuristic, then not forwarded before forwarded, then earlier reservation legs before later ones, then higher score, then earlier position. They are taken in that order while the row stays within 32 items and 16 KiB of JSON. An item too large for the remaining room is skipped, and smaller lower-ranked items may still fit. The kept items keep their original order.

### `extract_fp`

The hex BLAKE3 hash of these lines:

```
insights=<VERSION>
html-scan=<holtmail_html_scan::VERSION>
auth-results=<holtmail_auth_results::VERSION>
links=<holtmail_links::version()>
<id>=<version>      one per extractor, in run order
```

Each extractor's version is read once, in `Runner::new`. `holtmail_links::version()` embeds `holtmail_links::VERSION` and the hash of its tracking-parameter and tracker lists; the Public Suffix List (`psl_version()`) is not in it, since only stage B and the API read registrable domains. `VERSION` is `4` since the auth facts count `From` addresses (Phase 38); it was `3` for the merge keys and IATA normalisation (Phase 37); it was `2` for action normalisation and the structured-action merge rule (Phase 36). The input is marked `truncated` when the HTML scanner reports `truncated_structured` (a JSON-LD or microdata cap left something out), not when it merely returns 8 blocks.

### Stage B

```rust
pub const DERIVE_VERSION: &str = "1";
pub const ALWAYS_ON_SOURCE: &str = "signals";
pub struct DeriveConfig { pub health: bool, pub sources: Vec<String>, pub import_authserv_ids: Vec<String>,
                          pub smtp_authserv_ids: Vec<String>, pub smtp_account_name: String } // Default: health on, sources ["signals"], empty lists and name
Deriver::new(categorizers: Vec<Arc<dyn Categorizer>>, config: DeriveConfig) -> Deriver
Deriver::config(&self) -> &DeriveConfig
Deriver::policy(&self) -> CategoryPolicy                 // { health: config.health }
async Deriver::context(&self, store: &dyn Store, archive: ArchiveId) -> Result<DeriveContext, StoreError>
Deriver::context_from(&self, archive, accounts: &[Account], stored: &BTreeMap<AccountId, Vec<String>>, own_aliases: &[String]) -> DeriveContext
Deriver::changed(&self, ctx: &DeriveContext) -> bool
Deriver::derive(&self, ctx: &DeriveContext, row: &DeriveRow) -> CategoryWrite
DeriveContext::{archive() -> ArchiveId, derive_fp() -> &str, trusted_authserv_ids(AccountId) -> &[String], is_own(&str) -> bool}
pub struct AuthSummary { pub authserv_ids: Vec<String>, pub from_fields: usize, pub unsubscribe_fields: usize, pub unsubscribe_post_fields: usize }
pub fn auth_summary(auth_json: &str) -> AuthSummary
pub fn suggest_authserv(facts: &[String]) -> Option<String>
pub fn override_key(scope: OverrideScope, raw: &str) -> Option<String>
```

- **Voting categorizers.** `Deriver::new` keeps the categorizer with id `signals` and those whose id is in `sources`, in the given order; any other never votes and is not in the fingerprint.
- **Context.** `context` reads `list_accounts`, `trusted_authserv_ids` and `own_alias_addresses` and takes each voting categorizer's snapshot once. An import account's trusted list is `smtp_authserv_ids` when its name is `smtp_account_name` (non-empty), else `import_authserv_ids`; every other account's is its stored list. Ids are trimmed, lowercased, sorted and unique. Own addresses are the accounts' addresses and the own alias addresses, lowercase with an A-label domain. The server takes one context per pass and checks `changed` after each batch: it is `true` when a categorizer's current snapshot version differs, and the pass then starts again with a new context.
- **`derive` is pure.** No I/O, no clock, deterministic, never panics. Unreadable auth facts (bad JSON or another `v`) and unreadable items read as empty.

**Trust**, decided in this order (INSIGHTS §4.3):

1. Any alias row: `relayed { provider }` when the service signed it, else `unverified`. Never `aligned`, and `trust_authserv` is `None`.
2. The message's trusted set is the union, ASCII case-insensitive, of the lists of `row.accounts`. With no recorded `Authentication-Results` result whose authserv-id is in it: `unknown`.
3. Only the topmost trusted result (lowest `pos`) is read; every other result is ignored, so a forged field above it from another id changes nothing. `trust_authserv` is its id, lowercase. It gives `aligned { domain }` (the signing domain, lowercase) when `fields.from` and `from_addresses` are both 1 and `from_domain` is recorded, `sig_cap_hit` is false, and one of its DKIM passes has a signing domain (`d`, else the domain of `i`) whose registrable domain equals the From domain's, with at least one stored signature matching that pass and none of the matching ones carrying `l=`. Otherwise `unverified`.

A signature matches a pass when its `d` equals the pass's signing domain (ASCII case-insensitive), its `s` equals the pass's `s` when the pass recorded one, and its `b` starts with the pass's `b` when the pass recorded one.

**`one_click_ok`** (INSIGHTS §7.2) is `true` only for `aligned` messages whose topmost trusted result has at least one matching signature, over all its passes, where every one of them signs both `list-unsubscribe` and `list-unsubscribe-post` and has no `l=`; `sig_cap_hit` is false; and `fields` counts exactly one `List-Unsubscribe` and one `List-Unsubscribe-Post`. Registrable domains are never used for coverage.

**Sender.** The alias row's `sender` on an alias hit, else the stored From address, as lowercase with an A-label domain; `sender_domain` is its registrable domain. `own` is the From address (never the alias sender) being an own address.

**Categories.** The `CategoryInput` holds the stored signals and kind bits, the reservation types of the stored items that are `Likely` or higher and not forwarded, `structured` and `forwarded` from the signal bits, `own`, the trust and the sender. Every snapshot of the context votes. The kind is the vote of the highest `CategorySource::rank` naming one (the earlier categorizer on a tie), else `personal` from headers. The topics are the union of the votes, each with the highest-ranked source naming it, then `TopicSet::gated(trust, health)`: sensitive topics only for `aligned`, `health` only when the flag is on, unknown bits never. `derived_sources` lists the kept topics only.

**`derive_fp`** is the hex BLAKE3 hash of these lines:

```
derive=<DERIVE_VERSION>
psl=<holtmail_links::psl_version()>
taxonomy=<holtmail_category::TAXONOMY_VERSION>
health=<true|false>
sources=<sorted, unique, comma-separated>
<id>=<snapshot version>    one per voting categorizer, in order
trust=<BLAKE3 of "<account id>=<ids joined by commas>\n" per account, by account id>
own=<BLAKE3 of each own address and "\n", sorted>
```

Overrides are not in it: the store applies them by key.

**Suggested authserv-id.** `suggest_authserv(facts)` takes the auth facts as `CategoryStore::recent_auth_facts` lists them (most recently archived first). Of the first 200 that record at least one result, it counts the topmost result's id (lowercase), and returns it when at least 50 such facts exist and it is topmost in at least 95% of them. It is never used for trust. The server computes it only for synced accounts with an empty list, never for import or SMTP accounts.

**`auth_summary`** lists the recorded authserv-ids topmost first and the `From`, `List-Unsubscribe` and `List-Unsubscribe-Post` counts, for the account editor; it decides nothing.

**`override_key`** (INSIGHTS §4.5): trimmed, lowercase, the domain as an IDNA A-label without a trailing dot, a `+tag` kept. An address needs exactly one `@`, a non-empty local part without whitespace and a domain with at least two labels. A domain key is the registrable domain of the host, and an address is accepted there too (its domain is taken). `None` for anything else, an IP literal or a bare public suffix included.

### Panic hook

- `install_panic_hook()`: `apps/server` calls it at start. It chains the previous hook and installs only once.
- Inside an extraction scope it logs one `tracing` error naming the extractor id, version, message id and `file:line`. It never logs the payload, which can quote message content (an `assert!` or `expect` message that formats a value).
- Outside a scope it defers to the previous hook.
- `with_message(id, f)` sets the message id the report names for the duration of `f`, on this thread.

### `NormalizedText`

`NormalizedText::new(original, max_bytes)` is the NFKC step `input` uses. `original_offset(byte)` maps a byte offset in the normalised text back to the start of the original run it came from. The text is cut only before a character that is a stable NFKC boundary (combining class 0 and quick check Yes), or after 32 characters with no such boundary. So the kept text normalises exactly as the whole text would, except inside pathological runs of more than 32 combining characters. Runs that are ASCII or already pass the NFKC quick check are copied without normalising.

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
- a merge key per kind (same key joins, another flight number, day, station, start, carrier or attachment does not), the provenance order JSON-LD over microdata over pkpass over heuristic on one parcel in both run orders, then confidence, score and position, the heuristic check digit and tracking URL kept, `forwarded` only when every part was, IATA normalisation of flights and boarding legs, and a row of the previous items shape still reading;
- unwrapping and tracker flags on actions, a dropped over-long destination, the look-alike wrapper host left as is; a heuristic action yielding to a structured one of the same kind or destination, but not to a forwarded one; a code kept next to a forwarded reference of the same value; a code equal to a parcel number dropped;
- `tests/fixtures.rs`: `smtp-forged-ar-no-trust.eml`, whose forged `Authentication-Results` is recorded as written (trust is stage B's decision); and the fixtures of `crates/insight-links/tests/fixtures` plus `link-google-url.eml` through the full runner: destination host, `via` (`outlook.com`, `proofpoint.com`, `google.com`), `tracked` on a SendGrid click link, the look-alike wrapper host staying identity, and no action for the unsubscribe, forwarded, footer-only and newsletter cases; and `items-over-cap.eml`, sixteen JSON-LD flights with long passenger names, where the row cap keeps the earliest flights in order and drops the later legs.

- `src/derive/tests.rs`: every trust state and its order (alias first, signed and unsigned; unknown without a trusted id, for import and SMTP accounts and for unreadable facts; the topmost trusted result only, a forged untrusted one above it ignored; exactly one From field holding exactly one address; `header.i`; registrable alignment; no matching signature by `d`, `s` or `b`; `l=`; `sig_cap_hit`), coverage (both fields in `h=`, one field each, the selector narrowing, every candidate of the result), the topic union and kind precedence with `FakeCategorizer`, the content kind gate and `own`, the sensitive gate and the health flag, a source that is not enabled, `derive_fp` changing with each input and not with order or case, `changed` after a table swap with old contexts keeping their answers, `suggest_authserv` at 49/50 messages and 94%/95%, the 200 window, `auth_summary` and `override_key`.
- `tests/trust.rs`: `smtp-forged-ar-no-trust.eml` through stage A and B is `unknown` with an empty SMTP list and `unverified` once its id is trusted; `finance-spoofed-unverified.eml` (of `category-signals`) loses `finance` when unverified and keeps it when a matching signature is prepended; `finance-override-spoofed.eml` through `store-sqlite`: a domain override to `finance` applies to the aligned copy only.
- `tests/corpus.rs`: the precision gate below.
- `tests/golden.rs`: the golden snapshot below.
- `tests/explain.rs`: for every corpus message, the `explain` report contains no host, no `@`, no labelled code, unsubscribe link, link host or tracking number, and no wrapper or tracker name; and no labelled structured reference; and it names the expected rules for an origin-bound code, a code next to an order number, a one-click newsletter, an unwrapped reset link, a UPS parcel, a JSON-LD flight, a boarding pass and a code dropped for equalling a booking reference.
- `tests/bounds.rs`: a test-only counting `GlobalAlloc` (the only `unsafe` in the crate; the library keeps `#![forbid(unsafe_code)]`, and the crate's lint table denies rather than forbids `unsafe_code` for that reason) checks that no single allocation exceeds 32 MiB while the runner, with every built extractor, handles the shared hostile corpus and nine hostile messages (8 MiB of one token, 200,000 code lines, 1.6 MB of nested `<div><b>`, 20,000 anchors, a million combining marks, 4,000 header fields, eight JSON-LD blocks of `@id`-chained graphs, 2 MiB of microdata `itemref` fan-in, and a pkpass declaring 11 MiB). In release builds it asserts the whole path, `Runner::extract` with input building and the HTML scan, best of 3: at most 40 ms, or for a message with HTML twice the bare html5ever tokenizer on the same HTML when that is larger, because a loaded machine slows the tokenizer as much as the scan (quadratic work still fails by a wide margin). In debug builds it just has to finish.

## Corpus gate

`tests/corpus/` holds invented `.eml` messages (codes, action links and parcels in 12 languages, hard negatives, unsubscribe header variants, schema.org JSON-LD and microdata, pkpass attachments) and `labels.toml`, one `[[case]]` per file:

```toml
[[case]]
file = "otp-plain-en.eml"
code = "482913"                 # the code the card must show; absent means none
origin = "example.com"          # only for a WICG origin-bound code
negative = "order_number"       # the hard-negative family of a message without a code
unsubscribe = { https = "https://news.example.com/u/1", mailto = "mailto:leave@news.example.com", one_click = true, list_id = "weekly.news.example.com" }
links = [{ action = "reset_password", host = "accounts.example.com" }]   # primary first; host after unwrapping, A-label
parcel = [{ carrier = "ups", number = "1ZV77Y21K219503089" }]          # compact number as the carrier prints it
structured = [{ type = "flight", ref = "K4XB2Q" }]                    # see below
kind = "notification"                                                  # personal, list, bulk or notification
topics = []                                                            # never finance, health or government
```

`structured` lists the shown reservations, orders, invoices, parcels and passes from JSON-LD, microdata or a pkpass, one entry per item: `type` is `flight`, `lodging`, `train`, `bus`, `event`, `food`, `rental_car`, `order`, `invoice`, `parcel` or `pass`, and `ref` the reservation, order, invoice or tracking number, else the train or bus number, the name, the merchant or the payee; a pass by its organization. Structured actions are checked through `links`.

Every case also carries `kind` and `topics` (Phase 38). They are what INSIGHTS §4.2 gives for the message with trust `unknown`, so `topics` never holds a sensitive topic; `check_labels` enforces both fields and that rule.

`negative` may sit next to `links` or `parcel` when it names the code family of a message that has a link or parcel but no code.

`cargo test -p holtmail-insights --test corpus` runs the runner with the unsubscribe, schemaorg, pass, parcel, links and code extractors (in the server's order) over every file through `holtmail_mime::parse`, twice, and requires equal results. An item counts as found when it is `Likely` or higher and not forwarded, as the card and `has:` see it; `Possible` codes are listed separately.

| Kind | Rule | Gate |
|---|---|---|
| Codes | the one shown code equals `code` (and `origin`) | precision 0.98, recall 0.85 |
| Unsubscribe | the item equals `unsubscribe` field for field | precision and recall 1.0 |
| Links (top-1) | the highest-scored action has the action and destination host of `links[0]` | precision 0.95; recall printed |
| Parcels | every found parcel is in `parcel` (carrier and number) | precision 0.99; recall printed |
| Structured | the found structured items equal `structured` as a multiset of type and reference | precision and recall 1.0 |
| Kind | stage B with the `signals` categorizer, no trusted authserv-id and no own address gives `kind` | accuracy 0.97; confusion matrix printed |
| Topics | every derived topic is in `topics` | precision 1.0; recall printed |

The test prints a confusion summary (with a `structured` line) with case names and every wrong or spurious top link, and fails below the gates. It also requires at least 300 cases, at least 100 of them labelled `negative`, and exactly one label per file.

When the gate fails, fix the extractor's rules or the weights in its data file, not the labels, unless a label is wrong.

## Golden snapshot

`tests/golden.rs` renders the stage A row of every corpus message (outcome, `truncated`, from address, signal and kind bits, auth facts JSON, items JSON) under the current `extract_fp` and compares it with `tests/golden.txt`. Any drift fails, including drift from a dependency update (html5ever, regex, serde_json, url and so on):

- When the rows changed but `extract_fp` did not, stored rows would never be recomputed: bump the `VERSION` of what changed (`holtmail_insights::VERSION`, `holtmail_html_scan::VERSION`, `holtmail_auth_results::VERSION`, or the extractor's version), then rewrite the snapshot.
- When `extract_fp` changed as intended, review the diff and rewrite the snapshot.

Rewrite it with `UPDATE_GOLDEN=1 cargo test -p holtmail-insights --test golden`.

## Fuzzing

`fuzz/` is a `cargo-fuzz` crate with its own empty `[workspace]` and a `rust-toolchain.toml` pinning a dated nightly. Its `extract` target parses arbitrary bytes with `holtmail_mime::parse` and runs the runner with the unsubscribe, schemaorg, pass, parcel, links and code extractors twice, checking equal results and the row caps, then `explain`. Seed it with the corpus. With a Homebrew `cargo` first on `PATH`, put the pinned toolchain first:

```sh
cd crates/insights/fuzz
mkdir -p corpus/extract && cp ../tests/corpus/*.eml corpus/extract/
TC=$(dirname "$(rustup which --toolchain nightly-2026-10-01 rustc)")
PATH="$TC:$PATH" RUSTC="$TC/rustc" cargo-fuzz run extract -- -max_total_time=60 -rss_limit_mb=4096
```

`.github/workflows/fuzz.yml` runs this target nightly (and on `workflow_dispatch`) for 10 minutes, seeded with `tests/corpus`, next to the targets of `auth-results`, `html-scan`, `links`, `insight-schemaorg` and `insight-pass`; each job installs the nightly its folder's `rust-toolchain.toml` pins and a pinned `cargo-fuzz`, and uploads `artifacts/` when it finds a crash. A crash becomes a fixture.
