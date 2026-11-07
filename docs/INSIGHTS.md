# Message insights

Insights is what Holtmail reads out of a message so the reader can act on it at once. It covers one-time codes to copy, the action link to open, reservations, orders, parcels and passes, the unsubscribe options, and the message's kind and topics. Everything is derived from the raw bytes and stored once per message. The reader serves it with no blob read. Search can find messages by kind and category, never by value. The steps are WORKPLAN Phases 35–42.

## 1. Goals and non-goals

Goals:

- A card at the top of each expanded message in the reader. It shows what the user can act on, most urgent first, with one-click copy and a keyboard path to every action.
- Codes: one-time codes, shown with Copy and the `c` shortcut. Copy also works on plain-HTTP self-hosting.
- Links: action links (verify, confirm, reset, sign in, magic link, track, check in, save to wallet). Each shows its real destination domain, whether it goes through a tracker, and whether it leaves the sender's domain.
- Structured data: reservations, orders, invoices and parcels from schema.org markup, and passes from `.pkpass` files.
- Unsubscribe: the RFC 2369 links, plus RFC 8058 one-click as the only outbound action. One-click needs consent, a trusted DKIM result, and DKIM coverage of the exact header fields.
- Categories: one **kind** per message and zero or more **topics**, with user overrides that win on every axis they set. Categories work as a list filter, as counts in the sidebar and as search operators.
- Robustness:
  - Extraction is pure and deterministic, with bounded inputs, `catch_unwind` and a panic hook that never prints content.
  - Every item carries provenance, a confidence tier and a score.
  - Trust, alignment and categories are recomputed without re-parsing.
  - The backfill resumes and shares one parse per message with other derived data.
  - Data files are versioned and signed, and sensitive rows are reviewed.

Non-goals:

- No fetching during extraction or display. Links are never followed, prefetched or previewed, and there are no favicons.
- No DKIM verification of our own. Trust comes from the `Authentication-Results` field of an authserv-id the operator trusts (§4.3).
- Raw bytes are never rewritten. No header is stripped or stamped on ingest.
- Amounts are not read from free text. Money comes only from structured data, because a wrong total does more harm than a missing one.
- No second ICS parser. Invitations stay with `holtmail-pim` and `MessageDetail.invitations`, and the card only places them in its order.
- Attached `message/rfc822` parts are not extracted in this plan.
- These are Later items, each behind its own contract: BCBP parsing and barcode images (`holtmail-bcbp`, `holtmail-barcode`), a local classifier, and an LLM categorizer.
- No insight values go into webhooks, the index, logs, list payloads or settings. Only ids and counts appear in the `task.finished` event of an `analyse` task (§10).

## 2. Vocabulary

| Term | Meaning |
|---|---|
| insight | One extracted item (`Insight`): a code, an action link, a reservation, an order, an invoice, a parcel, a pass or the unsubscribe options |
| extractor | An `InsightExtractor`. It is pure, sync and versioned, and it never fails |
| runner | `holtmail-insights`. It builds the input, runs the extractors, merges, validates, caps, records auth facts and computes fingerprints. It also runs stage B |
| kind | Exactly one per message: `personal`, `list`, `bulk`, `notification` |
| topic | Zero or more per message, from a closed set: `purchases`, `travel`, `finance`, `health`, `social`, `jobs`, `events`, `government` |
| sensitive topic | `finance`, `health`, `government`. They need `aligned` trust from every source, overrides included |
| auth facts | Stage A's record of every `Authentication-Results` field (position, authserv-id, DKIM results) and every `DKIM-Signature` (`d`, `s`, `b` prefix, `h` counts, `l` present), plus header field counts |
| trusted authserv-id | An authserv-id the operator declared for an account, the import path or the SMTP listener. Only its `Authentication-Results` are read |
| trust | Per message, computed in stage B: `aligned`, `relayed`, `unverified` or `unknown` |
| stage A | Bytes to insights and auth facts. Needs a parse. Versioned by `extract_fp` |
| stage B | Stored stage-A columns, the stored alias row, trust lists, own addresses, data files and taxonomy, turned into trust, kind and topics. No blob read. Versioned by `derive_fp` |
| derived / effective | Derived is stage B's answer. Effective is derived with the user's override applied |
| walk | `apps/server/src/walk.rs`: one pass over blobs that parses each message once for every step that needs it |

The name avoids `MessageSummary` (the list row), `MessageFacts`/`message_facts` (Chats), `Extractor` (attachment text) and labels (user or Gmail labels). Categories are never stored as labels.

## 3. Modules

| Module | Kind | Job |
|---|---|---|
| `crates/insight` (`holtmail-insight`) | contract | `InsightExtractor`, `InsightInput`, `HtmlView` (`#[non_exhaustive]`), `Anchor`, `Segment`, `AttachmentMeta`, `Insight`, `InsightKind`, `Provenance`, `Confidence`, `PartRef`, `Url`, `LinkAction`, `Signals` (the persisted bit set of §4.4), `Reservation` and its seven structs, `Money`, `Currency`, `Date`, `LocalOrZoned`, `CarrierId`, `PassStyle`, `BoardingPass` (plain data, no parser), `KindTag`, `KindTags`, `InsightError`, plus `Items::to_json`/`from_json`. The `testing` feature holds `conformance::extractor` and the shared hostile corpus. No I/O, no parser, no merge logic |
| `crates/insights` (`holtmail-insights`) | plain library (like `holtmail-aliases`) | The runner. Stage A: input building with caps, header signals, auth facts, `catch_unwind` inside an extraction scope, normalise, merge, validate, caps, `extract_fp`, `install_panic_hook`. Stage B: `derive()`, trust, precedence, gates, `derive_fp`. Depends on the contracts, `holtmail-mime`, `holtmail-auth-results`, `holtmail-html-scan` and `holtmail-links`, never on implementations |
| `crates/auth-results` (`holtmail-auth-results`) | plain library, no dependencies | The RFC 8601 `Authentication-Results` grammar (authserv-id, `dkim=pass` with `header.d`, `header.i`, `header.s`, `header.b`, comments) and the `DKIM-Signature` tag-list grammar (`d`, `s`, `b`, `h`, `l`). Grammar only, no verification. Used by `holtmail-alias` and `holtmail-insights` |
| `crates/insight-code` | impl | One-time codes; `data/code-keywords.toml` (keywords, promo negatives, reference prefixes, weights) |
| `crates/insight-unsubscribe` | impl | RFC 2369/8058 from headers |
| `crates/insight-links` | impl | Action links; `data/link-actions.toml` (labels, path tokens, weights) |
| `crates/insight-parcel` | impl | Tracking numbers with check digits; `data/carriers.toml` |
| `crates/insight-schemaorg` | impl | JSON-LD and microdata: reservations, orders, invoices, parcels, actions; `data/currencies.toml` (ISO 4217 exponents) |
| `crates/insight-pass` | impl | `.pkpass` (`pass.json`, including `semantics`). BCBP parsing of the barcode message is Later |
| `crates/html-scan` (`holtmail-html-scan`) | plain library | Bounded HTML scan on the html5ever **tokenizer**, with an explicit element stack and rawtext/script-data states switched as the tree builder does. It skips hidden content and produces `HtmlView`: visible anchors, `ld+json` blocks, microdata items, visible text segments with emphasis, and the footer marker |
| `crates/links` (`holtmail-links`) | plain library (Phase 36) | `unwrap` (exact host allow-list), `strip_tracking` (our own CC0 parameter list), `tracker(url)`, `registrable_domain` (on `publicsuffix`), `display_host` (IDN policy), `VERSION`, `version()` (`VERSION` plus the hash of the parameter and tracker lists) and `psl_version()`. Chats §5.4 `links` reuses `unwrap` and `strip_tracking` |
| `crates/category` (`holtmail-category`) | contract | `Categorizer`, `CategorySnapshot`, `CategoryInput`, `CategoryVote`, `MessageKind`, `Topic`, `TopicSet` (with the trust gate `gated`), `TrustState`, `CategorySource`, `ReservationKind`, `ReservationKinds`, `CategoryError`, `TAXONOMY_VERSION`, conformance and `FakeCategorizer`. It depends on `holtmail-insight` for `KindTags` and `Signals`, and re-exports `Signals` |
| `crates/category-signals` | impl | Kind from header signals and insight kinds; topics implied by insight kinds and reservation types |
| `crates/category-data` (`holtmail-category-data`) | plain library | The TSV format: `parse`, `write`, `DomainTable` with bounded loading. The release manifest (Phase 42): `Manifest::{parse, peek_key_id, check, file, to_json}`, `ManifestFile`, `ManifestError`, the fixed `DATA_FILES` (file name, table name, license), `parse_version` and `is_newer`. Used by `category-domains`, `apps/category-builder` and `apps/server` |
| `crates/category-domains` | impl | Lookup by sender domain in the bundled CC0 table and downloaded tables. It holds the table internally and has a `reload` method that only `apps/server` calls. Release checks (Phase 42): the compiled-in `CURRENT_KEY` and `NEXT_KEY` slots (`ReleaseKey {id, public}`), `verify_manifest` (signature, key id, expiry, rollback), `verify_stored` (signature and key only), `check_file` (name, size, sha256, header, rows) and `UpdateError`; feature `test-key` adds `TEST_KEY` |
| `apps/category-builder` | tool binary, `publish = false` | Builds the data files from Wikidata and the UT1 mirror (`wikidata`, `ut1`, `check`), gates, writes manifests, signs and makes keys for releases (`gate`, `manifest`, `sign`, `keygen`). It depends on `holtmail-category-data`, not on an implementation. It is the only code that reaches Wikidata or UT1, and it runs in CI and by hand, never in the server |
| `holtmail-alias` | change | `Mail::dkim_pass` keeps its behaviour (`header.d=` or `header.i=@`, subdomains through `within`) but parses through `holtmail-auth-results`. No detector version changes |
| `holtmail-people` | change | Export `is_automated_local(&str) -> bool` (the pinned API, shared with Chats) |
| `holtmail-store`, `store-sqlite` | contract and impl | `InsightStore`; conformance area `insights` |
| `holtmail-fetch`, `fetch-http` | contract and impl | New traits `OneClick` (Phase 41) and `DataDownload` (Phase 42: `get(url, etag, max_bytes) -> Download {NotModified, Fetched {bytes, etag}}`; `HttpDataDownload` with `DataDownloadOptions` and `GITHUB_RELEASE_HOSTS`) next to `Fetcher`. `is_public` also refuses `2002::/16` when the embedded IPv4 is not public and `2001::/32` (Teredo); `64:ff9b::/32` is already refused |
| `holtmail-ingest` | change | `with_insights`, `refresh_insights`, `reindex_parsed`, and `refresh_alias` invalidates stage B |
| `holtmail-index`, `index-tantivy`, `holtmail-query` | change | Two fields, two clauses, operators |
| `apps/server` | composition root | `insights.rs` (builds extractors and categorizers, `[insights]` config), `category_data.rs` (the data `Updater` and its schedule), `walk.rs`, `derive_if_needed`, `WalkWake` |
| `@holtmail/ui` | change | `CopyButton`, `useCopy`, pure `copyText(env)` |

Decisions on the shape:

- There are two contracts because extractors and categorizers are the swap points (vendor extractors later, a local classifier, an LLM). They change for different reasons and cost different amounts to recompute.
- The runner, scanner, auth grammar and link library each have one correct behaviour, so they are plain libraries.
- Merge, validate, trust and precedence live in the runner, never in a contract crate (AGENTS.md).
- One exception: the topic gate `TopicSet::gated` (sensitive topics need `aligned`, `health` follows the flag, unknown bits are dropped) lives in `holtmail-category`. The store applies the same gate to overrides and may depend only on contracts, so the one pure gate sits where both reach it.
- Calendar data gets no extractor, because `holtmail-pim` already stores invitations.
- Schema-implied topics live in `category-signals`, because they read the same stored signals as the kind.
- The builder lives in this workspace, so the TSV format has one implementation (`holtmail-category-data`) and one CI. It is a plain library, so no second app depends on an implementation crate.
- The auth grammar sits in its own crate with no dependencies, so the `holtmail-alias` contract does not pull in `holtmail-mime` or `mail-parser`.

## 4. Data model

### 4.1 Contract types

```rust
pub trait InsightExtractor: Send + Sync {
    fn id(&self) -> &str;                       // "code", "unsubscribe", ...
    fn version(&self) -> String;                // includes blake3(data file)[..12] of every embedded table
    fn extract(&self, input: &InsightInput) -> Vec<Insight>;
}

pub struct Insight {
    pub kind: InsightKind,
    pub provenance: Provenance,   // Header | JsonLd | Microdata | PkPass | OriginBound | Heuristic
    pub confidence: Confidence,   // Certain | Likely | Possible: extractor semantics, part of the version
    pub score: u16,               // 0..=1000; Certain = 1000
    pub origin: PartRef,          // Header(name) | Subject | Text | Html | Attachment(ordinal)
    pub forwarded: bool,          // found below an inline-forward marker
}

pub enum InsightKind {
    Code { value: String, origin_bound: Option<String> },
    Action { action: LinkAction, url: Url, destination: Url, via: Option<String>, tracked: bool,
             text_host: Option<String>, label: Option<String> },
    Unsubscribe { https: Option<Url>, mailto: Option<String>, http: Option<Url>,
                  one_click: bool, list_id: Option<String> },
    Reservation(Reservation),     // Flight | Lodging | Train | Bus | Event | Food | RentalCar
    Order { number: Option<String>, merchant: Option<String>, total: Option<Money>, status: Option<String>, url: Option<Url> },
    Invoice { number: Option<String>, payee: Option<String>, total: Option<Money>, due: Option<Date>, url: Option<Url> },
    Parcel { carrier: CarrierId, number: String, check_digit: bool, track_url: Option<Url> },
    Pass { style: PassStyle, organization: String, description: String, relevant: Option<LocalOrZoned>,
           boarding: Option<BoardingPass>, attachment: u16, signature_checked: bool },
}

#[derive(thiserror::Error)]
pub enum InsightError { Unparsable, Decode(&'static str) }   // Display never carries content
```

Field rules:

- **`Url`** is the contract's own newtype. It takes `https`, `http` and `mailto` only, with no userinfo. Hosts are stored as A-labels (IDNA/UTS 46). A `url` longer than 8 KiB is rejected, and so is a `destination` longer than 2 KiB after unwrapping. Neither is ever cut.
- **Identifiers** (codes, references, order, invoice and tracking numbers) longer than 64 chars are rejected and never truncated. No output identifier or URL is a prefix of a longer input value.
- **Display text** (labels, merchant, organisation, description) is truncated at 512 chars on a char boundary, with an ellipsis.
- **`Money`** is `{ minor: i64, currency: Currency }`. `Currency` is an ISO 4217 code from `currencies.toml` with its exponent: JPY 0, USD 2, KWD and BHD 3. Unknown currencies are dropped. Prices parse as decimals with checked arithmetic. A price is rejected when it has an exponent, more than 18 digits, more fraction digits than the exponent allows, or overflows. Money is never a float.
- **`LocalOrZoned`** is `Local(NaiveDateTime)` or `Zoned(DateTime<FixedOffset>, Option<tz name>)`. Dates without an offset stay `Local` and are never shifted.
- **`BoardingPass`** is plain data (PNR, legs with airports, carrier, flight, date, seat). The future `holtmail-bcbp` produces it, and nothing in the contract parses.
- **`text_host`** is the host of the anchor text (A-label), kept only when the whole text is a host or an http(s) URL (`accounts.example.com`, `https://www.example.com/login`, `example.org:8443/a`); free text, addresses with userinfo, emails, numbers and IP addresses give none. `via` names the unwrapping service, for example `outlook.com`.

Doc-comment guarantees:

- Output is deterministic for equal input.
- `extract` never panics (the runner still catches panics), does no I/O and reads no clock. The "now" for date resolution is `input.sent_at`.
- Plain personal mail gives empty output.
- At most 16 items per extractor.
- No extractor depends on another. Cross-extractor rules (for example, a code equal to an order number) live in the runner's merge step, so each extractor's conformance suite covers its whole behaviour.

`InsightInput` mirrors `ParsedMessage`, and no `holtmail_mime` type crosses the contract:

- `headers`, `from`, `reply_to`, `subject`, `sent_at`.
- `text`: the first 256 KiB, NFKC-normalised for matching. Offsets map back to the original text.
- `forward_at: Option<usize>`: the char offset of the first inline-forward marker, using the CONVERSATIONS §6.3 marker list.
- `html: Option<HtmlView>`, built once from at most 2 MiB of HTML. Hidden content is already removed from anchors and segments; JSON-LD and microdata are kept inside hidden subtrees (§7.0).
  - `anchors`: at most 500. Each has `href`, `text`, `aria_label`, `button_like`, `in_footer`, `below_forward` and `index`.
  - `ld_json`: at most 8 blocks of 256 KiB each. `ld_json_forward_at: Option<usize>` is the index of the first block below the inline-forward marker; every later block is below it too, and `None` means no block is.
  - `microdata`: at most 4,000 items, item depth at most 16, at most 4,000 properties counted after `itemref` expansion. Each `MicrodataItem` has `below_forward`, set when its `itemscope` element is below the marker. Every `Item(i)` value is an index inside the list.
  - `truncated_structured`: set when any JSON-LD or microdata cap left something out. `false` guarantees `ld_json` and `microdata` hold everything the scanner read. List lengths alone do not show it.
  - `segments`: at most 4,000. Each has `text`, `emphasis`, `alone_on_line` and `below_forward`.
- `attachments`: metadata for each attachment. Content is included only for pkpass, at most 2 MiB each and at most 8 in all.
- The runner sets `truncated` on the row when `truncated_structured` is set, not when a list merely reaches its cap.

### 4.2 Categories

```rust
pub struct CategoryInput { pub signals: Signals, pub kinds: KindTags, pub reservations: ReservationKinds,
                           pub structured: bool, pub forwarded: bool, pub own: bool, pub trust: TrustState,
                           pub sender_address: Option<String>, pub sender_domain: Option<String> }
pub struct CategoryVote { pub kind: Option<MessageKind>, pub kind_source: CategorySource,
                          pub topics: TopicSet, pub source: CategorySource }
pub trait Categorizer: Send + Sync {
    fn id(&self) -> &str;
    fn snapshot(&self) -> Arc<dyn CategorySnapshot>;   // a consistent view: its table and its version
}
pub trait CategorySnapshot: Send + Sync {
    fn version(&self) -> String;                       // category-domains: every loaded table hash
    fn categorize(&self, input: &CategoryInput) -> Option<CategoryVote>;
}
```

`CategoryInput` holds no body. Every field is a stored column, or is computed in stage B from stored columns, the stored alias row, the archive's own addresses and the trust lists. It has no "people" input and no reply history, so categories can be reproduced from stored rows. `structured` is set when a non-forwarded `Certain` item from JsonLd, Microdata or PkPass is present.

- **`reservations`** holds the reservation types (flight, lodging, train, bus, event, food, rental car) of the items that `kinds` counts. The `reservation` bit alone cannot tell an event ticket from a trip, so stage B rebuilds the types from the stored items with the same filter as `kinds`. It is not persisted.
- **`kind_source`** is the source of the kind; `source` is the source of the topics. `category-signals` speaks for two layers, content and headers, so the two axes carry their own sources.

Precedence is applied separately on each axis, in the runner. For the kind, the highest layer that speaks wins. For topics, an override replaces the set; below it, topics are the **union** of content and domain file (each topic records its source in `derived_sources`), so an order from an aligned bank is both `purchases` and `finance`:

| Layer | Kind | Topics |
|---|---|---|
| 1. User override (§4.5) | yes | yes (replaces the set); sensitive topics need `aligned` |
| 2. Content (`category-signals`, from insight kinds at Likely or higher, non-forwarded) | `notification`, but only when there is content (`structured`, a reservation type, or an item kind other than `unsubscribe`) and the header kind is `bulk` or `notification` or `structured` is set; never when `own` or forwarded | flight, lodging, train, bus or rental-car reservation gives `travel`; event reservation gives `events`; food reservation gives none; order or parcel gives `purchases`; invoice gives `finance` |
| 3. Domain file (`category-domains`) | no | the topics in the file; none when `own` or forwarded. The file never holds consumer mailbox or ISP domains, and a site on a subdomain is keyed by its full host |
| 4. Headers (`category-signals`) | the kind rules below | no |
| 5. Later: local classifier, then LLM | fills gaps only | fills gaps only |

Kind from headers, in order:

1. `personal`: `own` (the From is one of the archive's own addresses, as CONVERSATIONS §4.1 defines "me"), or `Auto-Submitted: auto-replied` (bit `auto_replied`; an out-of-office is a person's mail).
2. `list`: `List-Id` with a `List-Post` other than `NO`.
3. `bulk`: `List-Unsubscribe` together with any of these: `Precedence: bulk|list|junk`, `Feedback-ID`, an ESP fingerprint (`X-SG-EID`, `X-MC-User`, `X-SES-Outgoing`, `X-Mailgun-*`, `X-PM-Message-Id`), or `List-Id` without `List-Post`.
4. `notification`: `Auto-Submitted` other than `no` and `auto-replied`, an `is_automated_local` sender, or an ESP fingerprint without `List-Unsubscribe`.
5. `personal`: everything else.

`List-Post` matches `NO` case-insensitively after comments are stripped, so `NO (posting not allowed)` counts.

The kind is deliberately not CONVERSATIONS §4.5's "automated". That rule decides whether a sender becomes a bot chat. The kind decides which inbox view a message belongs to. So `Precedence` alone or `List-Unsubscribe` alone gives `personal` here, because many transactional senders and people's own mail clients add them. Both features share `is_automated_local(&str)` and nothing else. CONVERSATIONS §4.5 is edited to say so (§16).

Content does not turn mail a person wrote into a `notification`. Nor does a forward of a receipt, or the user's own mail. A receipt sent through an ESP is `notification`. An unsubscribe item is not content: every newsletter carries one, so counting it would make every newsletter a `notification`.

Trust gates topics:

- Non-sensitive topics from the domain file and content apply at every trust level.
- A **sensitive** topic needs `aligned`, from every source, overrides included.
- So a spoofed `bank.example` never gets Finance, even after the user filed the real bank there.
- Unsigned mail, or mail with no trusted authserv-id, still gets Purchases or Travel.

Kind answers "who sent this and how", and topic answers "what is it about". So a travel newsletter is `bulk` plus `travel`, and `is:newsletter` still finds it.

`[insights.categories] health = false` removes `health` from every source, overrides included. Stored override bits are kept but not applied. The flag, the enabled source list and the taxonomy version all join `derive_fp`.

Config, `[insights.categories]`:

| Key | Default | Meaning |
|---|---|---|
| `health` | `true` | `false` derives, applies and serves no `health` topic |
| `sources` | `["signals", "domains"]` | The categorizers that vote: `signals` and `domains` (the bundled domain table). `signals` always votes; an unknown or repeated name stops the server at start |

The gate also drops topic bits this build does not know, because it cannot tell whether a newer topic is sensitive. The effective topics therefore hold known topics only; `derived_topics` and override `topics` keep every bit.

### 4.3 Trust

Trust is computed in **stage B**, from the stored auth facts, the stored `message_aliases` row and the trust lists. A change to alias detection, a trust list or the PSL therefore never needs a re-parse.

**Trust lists.**

- Each synced account has `trusted_authserv_ids`, stored in the `account_authserv` table (§5), so `Account` and `NewAccount` keep their fields. New accounts get a preset from the provider (Gmail, Fastmail, Outlook, iCloud, Yandex), read from the IMAP host, the JMAP address or the OAuth provider, and applied only while the list is empty. Presets are confirmed in VERIFY, and a preset stays empty until confirmed. The list is editable on the archive's account settings: at most 16 ids, each 1 to 255 bytes of printable ASCII, repeats ignoring case dropped.
- **Suggested id.** While a synced account's list is empty, the API computes a suggestion when it serves the account, from `recent_auth_facts` (the auth facts of up to 1,000 of the account's most recently archived messages, by archive order, never `sent_at`). Of the first 200 that carry an `Authentication-Results` field, the id in the topmost field of at least 95% wins, with at least 50 such messages. The suggestion is **never used for trust** on its own: a server that stamps 95% of the mail may still let a forged field with its id through (an upstream forwarder, a list MX), and only the operator can say it strips forgeries. The account editor shows it with "Trust this server", which copies it into the list. Import and SMTP accounts get no suggestion and no stored list. Computing it reads stored auth facts only.
- Imports have `[insights] import_authserv_ids`, empty by default.
- The SMTP listener has `[smtp] trusted_authserv_ids`, empty by default. Set it only to the authserv-id of the MTA in front of Holtmail, and only if that MTA removes forged fields with its own id, as RFC 8601 §5 requires. Stage B recognises the SMTP account as the import account named "Received by SMTP" (`smtp::ACCOUNT_NAME`); every other import account takes `import_authserv_ids`.
- Config ids follow the account rules: 1 to 255 printable ASCII characters without spaces, at most 16, none repeated ignoring case. A bad value stops every command at start and names the key.
- Raw bytes are never rewritten (AGENTS.md), so Holtmail strips nothing itself.
- A message's trusted set is the union over the sources where it has a location.
- The hash of each archive's effective lists (after the import and SMTP substitution, keyed by account id) joins `derive_fp`.

**Reading the field.** Only `Authentication-Results` fields whose authserv-id is in the trusted set are read. The topmost such field is used, and every other field is ignored. With no trusted authserv-id, or when the message carries no field from one, trust is `unknown`.

**States:**

States are decided in this order, and the first that applies wins:

1. **Alias hit.** Any stored `message_aliases` row decides alone: `signed` gives **`relayed { provider }`**, unsigned gives `unverified`. Alias mail never reaches `aligned`, because the service's signature aligns with its own rewritten From, not with the claimed real sender the lookups use. The service vouches for delivery, not for the original sender.
2. **`unknown`**: no trusted field to read (above).
3. **`aligned { domain, authserv }`** when all of these hold, else `unverified`:
   - the message has exactly one `From` field with exactly one address (`fields.from` and `from_addresses`, §4.4), because DKIM signs the bottom-most From while readers show the first;
   - the trusted field records `dkim=pass` with a signing domain: `header.d`, or when it is absent the domain part of `header.i` (some providers record only `header.i`; each preset's VERIFY entry records the form);
   - that domain's registrable domain equals the From address's registrable domain;
   - **at least one** stored `DKIM-Signature` matches it (by `d`, narrowed by `s` and `b` as in §7.2), and none of the matching ones has an `l=` tag. With no match, the answer is `unverified`, never a pass by default. A signature with `l=` lets anyone append content.

`unknown` says nothing is false, so the card does not warn on every message of an account whose server stamps nothing, or whose server the operator has not trusted yet.

What trust allows:

| | aligned | relayed | unverified | unknown |
|---|---|---|---|---|
| Card note | none | "via \<service\>, original sender not verified" | "Sender not verified" | "Sender not checked" in the footer, muted |
| Action links and schema.org actions | no trust badge (other badges still apply, §7.3) | warning badge | warning badge | no trust badge |
| Sensitive topics (any source) | yes | no | no | no |
| Other topics from the domain file | yes | yes (real sender's domain) | yes | yes |
| One-click unsubscribe | when covered | never | never | never |

**Sender.** The sender used for overrides and lookups is the alias's claimed real sender (`MessageAlias.sender`) for any alias hit, signed or not, and the From address otherwise. Signed or not changes only trust.

When `Ingest::refresh_alias` changes a message's alias answer, it calls `invalidate_derived([id])` and signals `WalkWake`. Stage B then recomputes trust, sender and categories for that message. When detection adds an alias address to the archive's own addresses, `derive_fp` changes and the whole archive is derived again, still without a blob read.

### 4.4 Header signals and auth facts

`Signals` is a bit set computed in stage A and stored. Stage B reads only these bits, so the header rules can change without a re-parse. Changing which bits are computed is a stage-A change.

**Persisted bit positions** are append-only, and a bit is never reused:

| Bit | `Signals` | `KindTag` (`kinds`) | `Topic` (`derived_topics`, `category_overrides.topics`) |
|---|---|---|---|
| 0 | `list_id` | `code` | `purchases` |
| 1 | `list_post` | `link` | `travel` |
| 2 | `list_post_no` | `unsubscribe` | `finance` |
| 3 | `unsubscribe` | `reservation` | `health` |
| 4 | `unsubscribe_post` | `order` | `social` |
| 5 | `precedence_bulk` | `invoice` | `jobs` |
| 6 | `auto_submitted` | `parcel` | `events` |
| 7 | `auto_replied` | `pass` | `government` |
| 8 | `feedback_id` | | |
| 9 | `esp` | | |
| 10 | `noreply` | | |
| 11 | `forward_marker` | | |
| 12 | `structured` | | |

**`auth` (JSON, stored):**

```json
{"v":2,
 "results":[{"pos":3,"authserv":"mx.example.org","dkim":[{"d":"news.example.com","i":null,"s":"s1","b":"AbC12345"}]}],
 "signatures":[{"d":"news.example.com","s":"s1","b":"AbC12345","h":{"from":1,"list-unsubscribe":2,"list-unsubscribe-post":2},"l":false}],
 "fields":{"from":1,"list-unsubscribe":1,"list-unsubscribe-post":1},
 "sig_cap_hit":false,
 "from_domain":"news.example.com",
 "from_addresses":1}
```

- `pos` is the field's index among all header fields.
- `b` keeps the first 8 characters.
- `h` counts how often each signed name occurs (DKIM oversigning).
- At most 8 results are kept, the topmost first. Signatures are kept when their `d` equals a `header.d` (or `header.i` domain) recorded as `dkim=pass` by any kept result, at most 8; the others are dropped first. `sig_cap_hit` is set when a matching signature had to be dropped, and stage B then gives `unverified` and no coverage, so prepended junk signatures cannot push the real one out.
- `fields` counts `from`, `list-unsubscribe` and `list-unsubscribe-post`.
- `from_domain` is the domain of the first `From` address; `from_addresses` counts the addresses of the parsed `From`.

### 4.5 Overrides

- **Scopes.** `address` (exact, after normalisation) and `domain` (a registrable domain). A subdomain cannot be overridden on its own in this plan: `news.example.com` and `example.com` share one domain override.
- **Key normalisation.** Lowercase, IDN to A-label, no trailing dot. A `+tag` is kept, so an address override is exactly the address.
- **Per-axis resolution.** For each axis (kind, topics), take the address override if it sets that axis, else the domain override if it sets that axis, else the derived value.
- **Gates.** Overrides apply at every trust level, but sensitive topics in an override apply only to `aligned` messages, and `health` follows the flag (§4.2). The chip menu shows "Not applied here: sender not verified" when an override's sensitive topic was held back on the open message.

## 5. Tables

The migrations take the **next free number at merge**:

- `00NN_insights.sql` (Phase 35, shipped as `0020_insights.sql`)
- `00NN_categories.sql` (Phase 38, shipped as `0021_categories.sql`)
- `00NN_insights_index.sql` (Phase 40, shipped as `0022_insights_index.sql`)
- `00NN_unsubscribe.sql` (Phase 41, shipped as `0023_unsubscribe.sql`)

If one of them lands before Chats Phase 0, the same change renumbers every CONVERSATIONS reservation to "next free at merge": §4.3 (l.215), §8.1 (l.527), the §8.2 heading (l.537), §8.4 (l.620) and §15 (l.1087, l.1110). A lower number never lands after a higher one has shipped.

```sql
-- 00NN_insights.sql
CREATE TABLE message_insights (
  message_id     TEXT PRIMARY KEY REFERENCES messages(id),
  archive_id     TEXT NOT NULL,
  fingerprint    TEXT NOT NULL,                 -- extract_fp
  outcome        TEXT NOT NULL CHECK (outcome IN ('ok','partial','unparsable','unreadable')),
  truncated      INTEGER NOT NULL DEFAULT 0,    -- some input hit a cap
  from_address   TEXT,                          -- lowercased, A-label domain
  signals        INTEGER NOT NULL,              -- Signals bits (§4.4)
  kinds          INTEGER NOT NULL,              -- KindTag bits at >= Likely, non-forwarded
  auth           TEXT NOT NULL,                 -- auth facts JSON (§4.4)
  items          TEXT NOT NULL,                 -- {"v":1,"items":[...]}; values included
  computed_at    INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX message_insights_fp ON message_insights(archive_id, fingerprint);

-- 00NN_categories.sql
CREATE TABLE category_overrides (              -- user state, never touched by recompute
  id TEXT PRIMARY KEY, archive_id TEXT NOT NULL,
  scope TEXT NOT NULL CHECK (scope IN ('address','domain')),
  key TEXT NOT NULL,                           -- normalised (§4.5)
  kind TEXT,                                   -- NULL = keep derived
  topics INTEGER,                              -- Topic bits; NULL = keep derived
  created_at INTEGER NOT NULL, created_by TEXT NOT NULL,
  UNIQUE (archive_id, scope, key)
);
CREATE TABLE message_categories (              -- the stage-B row
  message_id      TEXT PRIMARY KEY REFERENCES messages(id),
  archive_id      TEXT NOT NULL,
  derive_fp       TEXT NOT NULL,
  trust           TEXT NOT NULL CHECK (trust IN ('aligned','relayed','unverified','unknown')),
  trust_domain    TEXT,                        -- header.d (aligned) or the alias provider id (relayed)
  trust_authserv  TEXT,                        -- the trusted authserv-id used
  one_click_ok    INTEGER NOT NULL,            -- covered && aligned (§7.2)
  own             INTEGER NOT NULL,
  sender_address  TEXT, sender_domain TEXT,    -- registrable
  derived_kind    TEXT NOT NULL, derived_kind_source TEXT NOT NULL,
  derived_topics  INTEGER NOT NULL, derived_sources TEXT NOT NULL,  -- JSON {topic: source}
  kind            TEXT NOT NULL,               -- effective
  list_topic      TEXT,                        -- the one effective topic list rows show; never health
  override_ids    TEXT                         -- JSON {kind?: id, topics?: id}
) WITHOUT ROWID;
CREATE INDEX message_categories_fp      ON message_categories(archive_id, derive_fp);
CREATE INDEX message_categories_kind    ON message_categories(archive_id, kind, message_id);
CREATE INDEX message_categories_address ON message_categories(archive_id, sender_address);
CREATE INDEX message_categories_domain  ON message_categories(archive_id, sender_domain);
CREATE TABLE message_topics (                  -- effective topics, one row each
  message_id TEXT NOT NULL REFERENCES messages(id),
  archive_id TEXT NOT NULL, topic TEXT NOT NULL,
  PRIMARY KEY (message_id, topic)
) WITHOUT ROWID;
CREATE INDEX message_topics_filter ON message_topics(archive_id, topic, message_id);
CREATE TABLE account_authserv (                -- trusted authserv-ids of a synced account (§4.3)
  account_id TEXT PRIMARY KEY REFERENCES accounts(id),
  ids TEXT NOT NULL                            -- JSON array
);

-- 00NN_insights_index.sql
CREATE TABLE insight_reindex (                 -- durable queue: index document is behind
  message_id TEXT PRIMARY KEY REFERENCES messages(id),
  archive_id TEXT NOT NULL,
  seq        INTEGER NOT NULL                  -- bumped on every re-queue (UPSERT)
) WITHOUT ROWID;

-- 00NN_unsubscribe.sql
CREATE TABLE unsubscribe_requests (            -- audit of the only outbound action
  id TEXT PRIMARY KEY, archive_id TEXT NOT NULL,
  message_id TEXT,                             -- set to NULL when the message is deleted
  list_key TEXT NOT NULL,                      -- List-Id <...> content lowercased, else the stage-B sender_domain (registrable)
  requested_at INTEGER NOT NULL, requested_by TEXT NOT NULL,
  outcome TEXT NOT NULL CHECK (outcome IN ('accepted','redirected','refused','http_error','network')),
  status INTEGER
);
CREATE INDEX unsubscribe_requests_list ON unsubscribe_requests(archive_id, list_key, requested_at);
```

`list_topic` is the first effective topic in taxonomy order (purchases, travel, finance, social, jobs, events, government). `health` is never chosen.

**Items JSON.**

- The shape is `{"v":1,"items":[...]}`.
- Each item is `#[serde(tag = "type", rename_all = "snake_case")]` with the fields of §4.1 plus `p` (provenance), `c` (confidence), `score`, `origin` and `forwarded`.
- Reservations nest as `{"type":"reservation","r":{"type":"flight",...}}`.
- `from_json` is lenient. It skips an item that has an unknown type or does not deserialize, and then reports the row as `outdated`. It never fails the whole row.
- A store or runner test reads a row written in the previous shape.

The row states the API reports:

| State | Store | DTO `status` |
|---|---|---|
| Not computed | no `message_insights` row | `pending` |
| Computed, nothing found | row, `items.items = []` | `ready` |
| Stale | `fingerprint` differs from the current one, or `from_json` skipped an item. The old row is served until rewritten | `outdated` |
| An extractor panicked | `outcome = 'partial'`, items of the others kept | `partial` |
| Stored bytes no longer parse (written by the walk or reindex caller, e.g. after a mime upgrade) | `outcome = 'unparsable'`, not retried until the fingerprint changes | `unparsable` |
| Blob missing or unreadable | `outcome = 'unreadable'` with the current fingerprint, not re-listed until it changes | `unreadable` |

**Deletion.**

- `message_insights`, `message_categories`, `message_topics` and `insight_reindex` join the statement list in `store-sqlite/src/deletion.rs:98-107`, before `messages`.
- `UPDATE unsubscribe_requests SET message_id = NULL` runs in the same transaction, before `messages` is deleted, so the audit row stays.
- All six tables are deleted by `archive_id` in `delete_archive` (`lib.rs:196-231`), before `messages`.
- `account_authserv` rows are deleted with their account, in `delete_archive` before `accounts` and in `delete_account`.
- The `DeletionStore::delete_messages` doc (`crates/store/src/deletion.rs:30-35`) lists the four per-message tables.
- The ledger test's hand-deleted list (`lib.rs:2121-2129`) is unchanged. Its directly inserted message has no rows in the new tables, and `category_overrides` has no `message_id`.
- The `insights` conformance area ends with `delete_messages` and `delete_archive` assertions, as `aliases` does.

**`InsightStore`** (`crates/store/src/insights.rs`, added to `Store` and its blanket impl; the store treats `items` and `auth` as opaque JSON):

- `set_message_insights(message, &InsightRow)`: replaces the row whole. Returns `NotFound` for an unknown message.
- `insight_view(message) -> Option<InsightView>`: one call with at most 4 indexed reads. It reads the insights row, the categories row with its overrides and the topics. It does not read the last unsubscribe request: the store treats `items` as opaque JSON and cannot form the list key, so the API computes the key and calls `last_unsubscribe` (or `last_unsubscribes` once per thread).
- `insight_views_among(&[MessageId])`: the same for a thread, in batches of 200.
- `insights_among(&[MessageId])`: batches of 200, kinds and `list_topic` only, for list rows.
- `messages_needing_insights(archive, fp, after, limit)`: anti-join plus stale fingerprint, keyset-paged. `unreadable` and `unparsable` rows with the current fingerprint are excluded.
- The stage-B and override methods below form `CategoryStore` (`crates/store/src/categories.rs`), also part of `Store`.
- `derive_inputs(archive, derive_fp, after, limit)`: rows lacking a stage-B row, or with a stale `derive_fp`. Each comes joined with its `message_aliases` row and location sources. `derive_input(message)` gives one row for ingest, and `count_needing_derive(archive, derive_fp)` the count.
- `invalidate_derived(&[MessageId])`: deletes the stage-B rows of those messages.
- `set_message_categories(&[CategoryWrite], CategoryPolicy) -> Vec<MessageId>`: one transaction per batch of up to 500. It applies overrides in SQL and returns the ids whose **effective** kind or topics changed.
- `set_message_categories_queued(..)`: the same, and each batch queues its changed ids for reindex (as `queue_reindex`) in its own transaction, so a changed category is never committed without its queued reindex.
- `put_category_override(&CategoryOverride, CategoryPolicy) -> (CategoryOverride, Vec<MessageId>)`, `delete_category_override(archive, id, CategoryPolicy) -> Vec<MessageId>`: recompute the effective columns only for rows whose `sender_address` or `sender_domain` equals the key (§4.5), and return the changed ids. Both queue those ids for reindex in the same transaction.
- `category_overrides(archive)`.
- `category_counts(archive, include_health) -> (Vec<(Kind, u64)>, Vec<(Topic, u64)>)`.
- `trusted_authserv_ids(archive)` (every account of the archive, an empty list where none was set), `set_trusted_authserv_ids(account, &[String])`, and `recent_auth_facts(account, limit)` (up to 1,000, most recently archived first) for the suggestion (§4.3).
- `queue_reindex(&[MessageId])`: UPSERT, bumps `seq`.
- `reindex_queue(archive, after, limit) -> Vec<(MessageId, u64)>`.
- `dequeue_reindex(&[(MessageId, u64)])`: `DELETE ... WHERE message_id = ? AND seq = ?`.
- `count_reindex_queue(archive) -> u64`, for `/insights/status` and the deletion guard (§9).
- `record_unsubscribe(&UnsubscribeRequest)`: one row with a UUIDv7 `UnsubscribeRequestId`. `NotFound` for an unknown archive (always) or an unknown message; `Invalid` for a message of another archive or a used id; nothing is written on error. The 24 h and 1 min rules are the API's, not the store's.
- `last_unsubscribe(archive, list_key) -> Option<UnsubscribeRequest>`: the newest by `requested_at`, the greatest id among equal times, one indexed read; keys compare byte for byte.
- `last_unsubscribes(archive, &[String]) -> HashMap<String, UnsubscribeRequest>`: the same for many keys, 200 per query; keys without a request are absent. Conformance area `unsubscribe_requests`.
- `MessageExtras` gains `insight_kinds`, `kind: Option<MessageKind>` and `topic: Option<Topic>` (`list_topic`), read in the existing 200-id chunks of `message_extras` (`store-sqlite/src/lib.rs:753`).
- `MessageQuery` gains `category: Option<CategoryFilter>` (`Kind` reads the kind column, `Topic` joins `message_topics`).

## 6. Pipeline and versioning

### 6.1 Stage A in the runner

`holtmail_insights::Runner::extract(&ParsedMessage) -> InsightRow`:

1. **Input.** Build it with the caps in §4.1. Any cap hit sets `truncated`. The HTML is scanned once.
2. **Signals and auth facts** (§4.4), through `holtmail-auth-results`.
3. **Extractors** run in a fixed order: unsubscribe, schemaorg, pass, parcel, links, code.
   - Each call runs inside an extraction scope: a thread-local `IN_EXTRACTION` flag holding the extractor id and version, wrapped in `catch_unwind(AssertUnwindSafe(..))`.
   - A panic drops that extractor's output and sets `partial`.
   - The process panic hook, `holtmail_insights::install_panic_hook()`, is installed by `apps/server` at start and chained to the previous hook. Inside the scope it logs only the extractor id, version, message id and `file:line`, never the payload. Outside the scope it defers to the previous hook.
   - Extractor crates set `#![deny(clippy::string_slice, clippy::indexing_slicing)]`.
4. **Normalise.**
   - Every action's `url` goes through `holtmail_links::unwrap` (§7.3), with a loop depth of 4. `url` keeps the original, `destination` gets the unwrapped one, `via` names the wrapper's registrable domain (`google.com`, `outlook.com`, `proofpoint.com`), and `tracked` is set when `holtmail_links::tracker` knows the destination's host and path. An action whose destination is not a valid `Url` or is over 2 KiB is dropped. Nothing else is rewritten.
   - IATA codes are trimmed and uppercased. A flight whose departure or arrival airport is not three ASCII letters is dropped, a pass boarding leg with such an airport is removed from its pass, and an airline code that is not two ASCII letters or digits becomes `None`. Currencies need no step: the contract's `Currency` only holds an uppercase ISO code with its exponent.
5. **Merge.**
   - Codes: candidates are grouped by normalised value before scoring (§7.1).
   - Identical items are kept once. Items of one fact are then joined by the per-type merge key (§7.5), across all extractors. Numbers compare by their letters and digits, ignoring case; names and stations with whitespace collapsed, ignoring case; dates are the local date in the time's own offset.
   - The kept item has the higher provenance (JsonLd, then Microdata, then PkPass, then Heuristic), then the higher confidence, then the higher score, then the earlier position. It takes the place of the first item with its key, and it is `forwarded` only when every joined item was.
   - A kept parcel takes `check_digit` (OR) and a missing `track_url` from the parcels it absorbed, so a JSON-LD parcel keeps the heuristic check and carrier link.
   - Lists (passengers, seats, guests, tickets) are joined only inside `insight-schemaorg`, never across extractors.
   - A code equal to any reservation, order, parcel or invoice reference is dropped.
   - A heuristic action is dropped when a structured (JSON-LD, microdata or pkpass) action has the same `LinkAction` or the same destination.
   - A forwarded reference or structured action drops only forwarded codes and actions, never the sender's own.
6. **Validate.**
   - Drop items without their required fields (§7.5), or with an end before the start.
   - Drop money that does not parse (§4.1).
   - Cap the row at 32 items and 16 KiB of JSON. Under the cap, drop in this order: Possible before Likely, heuristic before structured, forwarded before not, later reservation legs last, then by score.

The walk and `Ingest::reindex` write `unparsable` when `holtmail_mime::parse` fails on stored bytes. The walk writes `unreadable` when the blob read fails. The runner itself never produces either state.

**Bounds that matter for CPU and stack.** Everything is deterministic: there are no time budgets and no `Instant`.

| Input | Bound |
|---|---|
| Text, HTML | 256 KiB, 2 MiB |
| HTML element stack | 256 (deeper elements are treated as flat); the tokenizer is iterative |
| JSON-LD | `serde_json` recursion limit 128, our walker depth 32, 2,000 nodes per message counted during expansion (JSON values, microdata items and property values together); `@id` resolved one level only, against top-level nodes, each id at most once per top-level node |
| Microdata | in the scanner: depth 16, 4,000 items, 4,000 properties counted during `itemref` expansion, a visited set per item, 64,000 property visits in all; in `insight-schemaorg`: depth 16 and the shared 2,000-node budget |
| Anchors scored | 500, each scored linearly |
| Code candidates | 64 distinct values, keyword positions 64 (at most 4,096 distance checks) |
| Regexes | `regex` crate only (linear time) |
| pkpass | 2 MiB compressed, 64 entries, `pass.json` ≤ 512 KiB, ≤ 10 MiB total uncompressed (declared sizes and a counting reader), no `..` paths |

Nothing recurses over input in our code. Stack overflow aborts the process instead of unwinding, so these caps are what keep extraction inside the stack.

**Determinism.**

- No clocks, no randomness, no locale, and no hash-map order in the output.
- The runner's corpus test runs every case twice and compares the bytes.
- A **golden snapshot test** (`crates/insights/tests/golden.rs`) serialises the rows of the whole corpus and compares them with a checked-in snapshot. Any drift fails the test, including drift from a `cargo update` of html5ever, regex, serde_json, zip, url or publicsuffix, and the fix is a `VERSION` bump with the new snapshot.
- The workspace sets no `panic = "abort"`. `holtmail-insights` has a `panics_unwind` test that CI also runs with `--release`, and `mise run check` fails if any profile sets `panic = "abort"`.

### 6.2 Fingerprints

| Fingerprint | Covers | Not in it |
|---|---|---|
| `extract_fp` | `holtmail_insights::VERSION` (merge, validate, caps, signal bits, auth-facts shape); `html_scan::VERSION`; `auth_results::VERSION`; `links::version()` (`links::VERSION` for the unwrap allow-list and decoders, plus the hash of the tracking-parameter and tracker lists); each `id=version` line, which embeds `blake3(data)[..12]` of every extractor data file | the PSL, trust lists, alias rows, thresholds, overrides, domain files |
| `derive_fp` | the runner's stage-B version (trust rules, precedence, gates); each categorizer snapshot `id=version`, with every loaded domain-file hash; the `publicsuffix` list version; the taxonomy version and config (`health`, `sources`); per archive, the hash of its trust lists and of its own-address set | overrides, thresholds |

Display policy:

- The card always shows items at `Likely` or higher. `[insights] show_possible.<kind>`, off by default, adds `Possible` items scoring at least the given value. It is read when a request is served.
- List glyphs, `kinds` bits and `has:` all use `Likely` or higher. So a glyph or a `has:code` hit always has a visible card item. One exception: the `link` bit cannot tell sign-in actions from `view_order` or `track`, so a member's row keeps `link` even when every action is filtered out for them (§10).
- Overrides are applied by the store, by key, and never mark anything stale.

The PSL is not in `extract_fp`. Alignment and sender domains are computed in stage B. `destination_differs_from_sender`, `text_domain_mismatch` and the display host are computed when the DTO is built, from the stored hosts. A PSL bump therefore costs one stage-B pass, never a re-parse.

### 6.3 Ingest

- **`Ingest::ingest`.** After `set_message_alias`, it computes insights when `inserted.created`, **or when `message_insights` has no row for the id**. A retried sync then repairs a failed write. The steps: `runner.extract(..)`, then `set_message_insights`, then stage B for that one message with the current snapshots, then `set_message_categories`.
- `index_parsed` reads kinds and effective categories from the store, as it reads labels.
- `Ingest::with_insights(Arc<Runner>)` is wired in `Services::ingest()`.
- `Ingest::refresh_insights(id) -> bool` mirrors `refresh_alias`: it writes only on change and reindexes only when indexed fields changed.
- `Ingest::refresh_alias` calls `invalidate_derived` when the alias answer changed, and the server signals `WalkWake`.
- `Ingest::reindex_parsed(message, &ParsedMessage)` is a public wrapper over the private `index_parsed` (`crates/ingest/src/lib.rs:346`), for the walk's `index` step.
- **Reindex fills gaps.**
  - `Ingest::reindex` already reads and parses the blob. When the insights row is missing or stale, it computes and stores the row, and the stage-B row, before building the document.
  - The single schema-bump reindex (Phase 40) therefore also completes any insights backfill, and the walk then finds nothing to do.
  - Reindex stays the existing global `REINDEX_SETTING` pass. It is not a walk step.

### 6.4 The walk

`apps/server/src/walk.rs` has a server-private trait:

```rust
#[async_trait]
trait Step: Send + Sync {
    fn name(&self) -> &'static str;
    async fn pending(&self, store: &dyn Store, archive: ArchiveId, after: Option<MessageId>, limit: u32) -> Result<Vec<MessageId>, StoreError>;
    async fn apply(&self, item: &WalkItem) -> Result<StepOutcome, anyhow::Error>; // WalkItem { message, raw, parsed }
    async fn committed(&self) -> anyhow::Result<()> { Ok(()) } // after each page's index commit
}
```

How it runs, per archive:

- It unions the `pending` ids of every step in pages of 200, ordered by id.
- It reads each blob once and parses it once, two messages at a time.
- It runs every step that listed the id, commits the index per page and sleeps 50 ms between pages.
- A deleted message (`NotFound`) is skipped. A failed blob read writes `unreadable` and goes on, and a parse failure writes `unparsable`. Neither is listed again until the fingerprint changes, so no loop spins.
- It needs no cursor to resume, because each step's `pending` is its own anti-join. A walk killed with SIGKILL picks up exactly where its rows end.

**Triggers.**

- The walk runs once in the start-up chain, **after `people::build_if_needed`** (`main.rs:1315-1323`; CONVERSATIONS §8.7).
- It then waits on `WalkWake`, a coalescing notify like `PeopleRefresh`. These signal it: override edits, the stage-B pass, data-file loads, `refresh_alias` changes and deletion reviews (§9).
- A timer also signals it hourly, so the anti-joins catch rows lost to a crash between `insert_message` and `set_message_insights` while the server runs.
- `derive_if_needed` (§6.5) runs as part of each wake, before the `index` step drains the queue.

**Progress.**

- Progress shows as `TaskKind::Analyse` in `/api/tasks`, with counts only.
- There is one task per walk run, created only when the union of pending ids is non-empty.
- Its `task.finished` webhook carries kind `analyse` with ids and counts only (§10).
- Progress is also published to an in-process `InsightProgress` handle for `/insights/status`.

**Steps:**

- `insights` (Phase 35).
- `index` (Phase 40): reads `(id, seq)` from `insight_reindex`, calls `Ingest::reindex_parsed` with `WalkItem.parsed`, and in `committed`, after the page's index commit, calls `dequeue_reindex` with the same `seq`. A re-queue in between leaves the row, and a run that stops before the commit leaves the whole page queued. A message whose blob cannot be read or parsed is dequeued too.
- `chat-facts`: when Chats lands, its §8.7 backfill becomes this step, and the `chat_state.facts_done` cursor is dropped. CONVERSATIONS.md is edited in the same change that lands the walk.

### 6.5 Stage B pass

`derive_if_needed` runs after the start-up walk and on every `WalkWake`.

- It takes snapshots once per pass: each categorizer's `snapshot()`, the trust lists, and the own-address sets. From them it computes `derive_fp`.
- It pages `derive_inputs` 1,000 at a time and runs `Runner::derive` against those snapshots, with no blob read. It writes the `derive_fp` it computed with.
- If a categorizer's version changes during the pass (a data file swapped), the pass stops after its current batch and starts again with fresh snapshots. Rows written so far carry the old fingerprint and are picked up again. Archives finished earlier in the pass are done again in the same pass.
- An archive that restarts more than 8 times, or a pass that needs more than 8 extra rounds, is left for the next wake, so a categorizer that keeps changing cannot loop forever. A store error in one archive is logged with its id, and the pass goes on to the others.
- It writes with `set_message_categories_queued` in batches of 500. Each batch is one transaction that also queues the ids whose effective categories changed, so the writer mutex is held briefly.
- When the pass queued anything, the walk runs again at once for the `index` step, without waiting for a wake.
- Ingest's single-message stage B takes the same snapshots per call and writes their fingerprint.

**Override edits** are SQL only, under an `overrides.rs`-style `CHANGES` mutex in `holtmail-api` (`categories.rs`), the only writer of overrides:

- A `put` or `delete` recomputes the effective columns for the rows matching the key. Deleting an override restores the derived values, then applies any remaining override per axis.
- The changed ids are queued for reindex in the same store transaction, and `WalkWake` is signalled.
- The list filter and counts are exact as soon as the request returns.
- `category:` search catches up as the `index` step drains the queue, at about 4,000 messages a minute on SSD.
- The card and the sidebar link to SQL filters, never to search, so the lag shows only in typed searches.

## 7. Extractors

### 7.0 HTML scan and forwards

**Tokenizer states.** `holtmail-html-scan` returns `TokenSinkResult::RawData`/`Script` from its sink, as the tree builder does:

| Element | Tokenizer state |
|---|---|
| `title`, `textarea` | RCDATA |
| `style`, `xmp`, `iframe`, `noembed`, `noframes`, `noscript` | RAWTEXT |
| `script` | script data |
| `plaintext` | PLAINTEXT |

The text of RAWTEXT, script-data and PLAINTEXT elements is never a segment. `<script type="application/ld+json">` feeds `ld_json`. Each element has a fixture.

**Hidden content.** These subtrees are skipped: `hidden`, `aria-hidden=true`, inline `display:none`, `visibility:hidden`, `font-size:0`, `opacity:0`, `max-height:0` with `overflow:hidden`, `<template>`, `<noscript>`, and every element `holtmail-sanitize` (ammonia) strips. Anchors and segments inside them are not reported.

**Structured data is metadata.** Senders often hide it, so JSON-LD blocks and microdata are collected inside CSS-hidden and `aria-hidden` subtrees, and JSON-LD in `<head>` too. Neither is ever collected inside `<template>`, foreign content (SVG, MathML), raw-text elements or elements the sanitizer drops with their content.

**Microdata** follows WHATWG microdata, flat, in the document order of the `itemscope` elements:

- `itemtype` is split on ASCII whitespace and `itemid` is kept as written. An item is `top_level` when its element has no `itemprop`.
- Values come from the usual attribute table (`content` on `meta`, `href`/`src`/`data`, `datetime` on `time`, `value` on `data` and `meter`, otherwise text content). A missing attribute gives an empty value. Text is trimmed and cut to 512 chars; text content includes hidden text but not template text, with whitespace collapsed.
- `itemref` is resolved after the token stream, without recursion. Each id counts once per item, properties are deduplicated per item, and a property whose value is the item itself is cut. Other cycles can remain; consumers walk with their own visited set.
- An `itemscope` deeper than 16 is a plain element: its `itemprop` takes its text content and its descendants' properties go to the nearest kept item.
- `itemscope` on `<html>` or `<body>` makes the item that stray properties belong to; `itemprop` and `id` on those two tags are ignored. `itemprop` on a raw-text element counts only on `iframe` (its `src`).
- A URL value over 8 KiB is left out, never cut, so expansion never copies large values.
- Hitting any cap (the 4,000 items, properties or depth, the 64,000-visit expansion budget, an over-long URL, an `itemscope` or text-valued `itemprop` past the 256-deep element stack, a ninth or over-long JSON-LD block) sets `truncated_structured`.

**Differential fuzz.** A target checks that every anchor `href` and every segment text the scan reports also survives `holtmail-sanitize` on the same input.

**Forwards.**

- Inline-forward markers (the CONVERSATIONS §6.3 list) set `forward_at` and the `below_forward` flags.
- Codes and heuristic actions are taken only from content above the marker.
- Structured items found below it are kept with `forwarded: true`. The card labels them "Forwarded", and they never set the content kind or topics.
- Attached `message/rfc822` parts are not extracted in this plan.

**Own mail.** Code and action items are never shown on own messages. The DTO drops them. Stage A still stores them, because `own` can change. Until stage B exists (Phases 35–37), the DTO computes `own` at read time from the archive's account and alias addresses; from Phase 38 the reader reads the stored `own`. List rows keep the read-time rule, because `MessageExtras` carries no `own`.

### 7.1 Codes (`insight-code`)

**Matching mode.**

- Text and keywords are NFKC-normalised, with Unicode simple case folding. Fullwidth digits become ASCII, and that ASCII form is the stored value.
- Keywords match whole-word in spaced scripts and as substrings in CJK and Thai.
- A token is a run of letters or digits, or a single CJK character.
- All windows are measured in chars.

**Source text.**

- When HTML is present, candidates come from `segments`, and positions are computed over the concatenated segment text.
- Otherwise candidates come from `text`, with `alone_on_line` taken from line breaks.
- A value found in both parts gets a bonus. When the parts disagree, the HTML wins.

**Steps:**

1. **Origin-bound.** A WICG `@origin #code` last line in text is `Certain`, with `origin_bound` set.
2. **Gate.**
   - The message needs a keyword hit from `code-keywords.toml` in the subject or the first 4,096 chars. Languages: en, ru, de, fr, es, pt, it, zh, ja, ko, tr, pl.
   - It must have no `List-Id`, unless `Auto-Submitted` is present.
   - The alphanumeric shape is used only when no `bulk` header signal is set (`precedence_bulk`, `feedback_id`, or `esp` together with `unsubscribe`).
3. **Candidates.**
   - Runs of 4–8 ASCII digits. Boundaries are checked in code: the chars before and after must not be ASCII alphanumeric. CJK neighbours are allowed, so `验证码是123456` works.
   - `\d{3}[ -]\d{3}` (ASCII), normalised.
   - 6–8 ASCII alphanumerics with at least one digit and one letter: not all letters, not a hex colour, not a word from the keyword table.
4. **Group** candidates by normalised value. Each value takes its best occurrence's score plus 25 per extra occurrence, up to 75.
5. **Score**, with the weights in `code-keywords.toml [weights]` (part of the version; initial values, tuned by the corpus gate):

   | Signal | Points |
   |---|---|
   | base | 300 |
   | keyword distance within 80 chars | +300 at 0 chars, falling linearly to 0 at 80 |
   | in subject | +150 |
   | `emphasis` (`strong`, `b`, `h1`–`h3`, letter-spacing, large font) | +150 |
   | `alone_on_line` | +100 |
   | within the first 1,024 chars | +50 |
   | also in the other part | +50 |
   | alphanumeric shape | −100 |

   Scores are clamped to 0..=1000.
6. **Hard negatives** (score 0). Each is a named rule with a fixture:
   - `year_near_month`: 1900–2099 within 3 tokens of a month name, or in a date shape. A bare 4-digit 1900–2099 value without a month nearby takes the `year_like` weight (−150) instead.
   - `money`: a currency symbol or ISO code within 3 tokens, or a decimal part.
   - `phone`: a leading `+`, or a phone shape of 10 or more digits.
   - `postcode`: next to address words.
   - `reference_prefix`: `#`, Order, No., Nº, Invoice, Ticket, Tracking, Заказ, Bestellung, plus the per-language list.
   - `promo_keyword`: within the window of promo, coupon, discount, voucher, gift card, referral, Gutschein, промокод, 优惠码 or the per-language list.
   - `in_url`: inside `href`, `src`, `style` or a URL in text.
   - `footer`: after the footer marker (an unsubscribe anchor or `©`).
   - `forwarded`: below the forward marker.
7. **Output.**
   - At most one code: the best value, only when it beats the best *different* value by 150 points. Ties emit nothing, and so do backup-code lists.
   - It is `Likely` at a score of 600 or more, otherwise `Possible` (700 at first; the corpus gate showed an inline "code is 482913" scores about 640).

### 7.2 Unsubscribe (`insight-unsubscribe`)

**RFC 2369, strictly.**

- Only `<…>` items count. They are comma-separated and read left to right. After a `>`, skip to the next comma.
- Stop at the first item that is not bracketed.
- Refuse `file:` and every scheme except `https`, `http` and `mailto`. Keep the first of each scheme.

**`one_click`** requires exactly one `List-Unsubscribe-Post` field whose value is exactly `List-Unsubscribe=One-Click` (trimmed, case-sensitive), and an `https` URI.

**Multiple fields.** When a message has more than one `List-Unsubscribe` field, the first is read for links, but one-click is never offered (below).

**Coverage**, computed in stage B into `one_click_ok`, from the auth facts:

- Use the trusted `Authentication-Results` field (§4.3). Take its `dkim=pass` results with their signing domain (`header.d`, else the `header.i` domain), and match `DKIM-Signature` entries whose `d=` equals it exactly (case-insensitive).
- Narrow the match by `s=` when `header.s` is recorded, and by the `b=` prefix when `header.b` is recorded.
- Every remaining candidate must name both `list-unsubscribe` and `list-unsubscribe-post` in `h=`, as RFC 8058 requires. Oversigning is not required: most bulk senders do not oversign, and requiring it would hide one-click on nearly all real mail.
- Every candidate must have no `l=` tag.
- **At least one** candidate must remain. No candidate, or `sig_cap_hit`, means not covered.
- The message must have exactly one `List-Unsubscribe` field and exactly one `List-Unsubscribe-Post` field. A prepended second field therefore withdraws one-click, which is the attack oversigning would otherwise stop.
- Registrable domains are never used for coverage.

**When one-click is offered:** only when `one_click && one_click_ok`, where `one_click_ok` already requires `aligned`. The card can say why the button is missing (§10).

**Confidence:** `Certain`.

**Hard negatives:** `List-Unsubscribe-Post` without an https URI, and `<https://…>` after an unbracketed item.

### 7.3 Action links (`insight-links`)

**Unwrapping** (`holtmail-links`), on an exact host allow-list only:

- `www.google.com/url` (`q`, `url`).
- `*.safelinks.protection.outlook.com`, matched as an exact suffix on a label boundary (`url`).
- `urldefense.com` and `urldefense.proofpoint.com` (v1–v3).

When a URL is unwrapped, `via` is set to the outermost wrapper's registrable domain (`google.com`, `outlook.com`, `proofpoint.com`) and the card shows "via outlook.com". When the decoder meets anything it does not understand, `destination = url` (fail closed). A proptest checks that `unwrap(x) == x` for every host not on the list, and fixtures cover look-alike hosts (`evil.example/url?q=`, `safelinks.protection.outlook.com.evil.example`).

**Scoring**, with the weights in `link-actions.toml [weights]` (initial values):

| Signal | Points |
|---|---|
| anchor text matches an action label (multilingual): `verify`, `confirm`, `activate`, `reset_password`, `sign_in`, `magic_link`, `view_order`, `track`, `check_in`, `wallet_save` | +400 |
| `aria_label` match | +300 |
| `button_like` (a background cell, padding or `role=button`) | +150 |
| path token (`/verify`, `/reset`, `/magic`, `token=`) | +150 |
| the first anchor with an action, in document order | +50 |
| a Google Wallet save URL (direct, not wrapped): the score is raised to at least this before the penalties | 600 |
| `in_footer` | −400 |
| a social domain (also behind a wrapper) | −300 |

Unsubscribe, preference and privacy URLs (also when wrapped), `mailto`, anchors below the forward marker and URLs over 2 KiB score 0. Scores are clamped to 0..=1000. Labels match the earliest place in the text, the longest label among those starting there; text and `aria-label` are read up to 512 chars.

**Output:** the top 2 with a score of at least 600 (the `likely` weight), which is `Likely`, highest score first, ties by anchor index; the second only when its URL differs from the first. The runner then unwraps them and sets `tracked` (§6.1). A structured action of the same kind or with the same destination outranks a heuristic one, which the runner drops.

**Read-time flags**, computed in the DTO from the stored hosts with `publicsuffix`:

- `text_domain_mismatch`: `text_host`'s registrable domain differs from the destination's.
- `destination_differs_from_sender`: the destination's registrable domain differs from the stage-B row's `sender_domain` (the alias's claimed sender for alias mail, §4.3), or from the From domain when there is no stage-B row yet. An aligned `trust_domain` shares its registrable domain with the sender, so it needs no second comparison. A destination with no registrable domain (an IP literal, a single-label host, a bare public suffix) differs unless its host equals the sender's.
- `confusable`: by UTS 39, set when any destination label is mixed-script or confusable (Chromium's IDN display policy). The card then shows the A-label form with a warning. Otherwise it shows the Unicode form.

**Wallet links:** a direct `pay.google.com/gp/v/save/…` link becomes `wallet_save` whatever its text, so an image-only button still counts. A wrapped one becomes `wallet_save` only through its label, so no link can pose as a wallet button by its URL. The JWT is not decoded.

**Trackers:** `tracked` covers ESPs whose click hosts are their own (SendGrid, Mailchimp, Mandrill, SparkPost, HubSpot, Customer.io, Amazon SES, Postmark, Mailjet, Constant Contact, Campaign Monitor, Klaviyo, and Salesforce Marketing Cloud on `click.exacttarget.com` and `*.exct.net`; the list is `crates/links/data/trackers.toml`). Mailgun and Braze track on the sender's own domain and are not recognised.

### 7.4 Parcels (`insight-parcel`)

**Check digits:**

| Carrier | `CarrierId` | Pattern | Check |
|---|---|---|---|
| UPU S10 | `s10` | `AA123456789CC` | mod 11, weights 8,6,4,2,3,5,9,7 |
| UPS | `ups` | `1Z` + 16 | mod 10 variant |
| USPS IMpb | `usps` | 20–22 digits | mod 10, weights 3,1 from the right |
| FedEx | `fedex` | 12 or 15 digits | 12: weights 3,1,7 from the left over 11 digits, sum mod 11 mod 10; 15: mod 10 |
| DHL Express | `dhl_express` | 10 digits | first 9 digits mod 7 |
| Russian Post | `russian_post` | 14 digits | mod 10, weights 3,1 from the right |

**Scores:**

| Evidence | Score | Tier |
|---|---|---|
| valid check digit and a context keyword nearby (`tracking`, `Sendungsnummer`, `трек`, …) | 850 | Likely |
| valid check digit and the sender host matches the carrier's hints | 900 | Likely |
| valid check digit, keyword and host hint | 950 | Likely |
| no check digit for the pattern, with both keyword and host hint | 500 | Possible |

Every carrier above has a check digit and a failed check gives nothing, so the 500 row does not apply to the bundled table; `no_check_digit` stays in `carriers.toml` for a carrier without one. Nothing else is emitted.

**Context rules:**

- A keyword counts within 64 chars of the number (`[context] keyword_window`), in the generic list (13 languages) or the carrier's own.
- An order, invoice, account, card or phone word within 24 chars before the number (`negative_window`) drops it, unless a tracking keyword before the number is closer. A leading `+` or an adjacent bracket also drops it.
- An anchor counts as context only when its `href` contains the number and its text or `aria-label` has a keyword.
- Numbers below the forward marker or in the footer give nothing. When two carriers match one number, the one with a host hint, then its own keyword, wins; a tie gives nothing.

**Tracking URLs** are built only from the template in `carriers.toml`, so the host never comes from mail.

### 7.5 schema.org (`insight-schemaorg`, Phase 37)

**What is read:**

- JSON-LD from `HtmlView.ld_json`: top-level arrays, `@graph`, and `@type` as a bare name, `schema:X` or an `http(s)://schema.org/X` URI. `@id` references are resolved one level deep, against top-level nodes only; references inside a resolved node are not followed, and each id expands at most once per top-level node.
- Microdata from `HtmlView.microdata`, with `Item` cycles and out-of-range indexes cut by a visited set.
- Only top-level nodes and their `potentialAction` are mapped. A root of an unknown type (Gmail's `EmailMessage`) still has its `potentialAction` read. Nested `orderDelivery` and `partOfOrder` give no second item.
- When the 2,000-node budget runs out, the top-level node being built is dropped and nothing after it is read.
- An item is `forwarded` when its JSON-LD block index is at or after `ld_json_forward_at`, or its microdata item is `below_forward`.

**Supported types:** the seven reservation types; `Order`, `ParcelDelivery`, `Invoice`; and `ConfirmAction`, `ViewAction`, `TrackAction` and `CheckInAction`, which become `Action` with `Provenance::JsonLd` or `Microdata`. A FlightReservation's `checkinUrl` also gives a `CheckIn` action.

**Parcels.** `carrier` (else `provider`) maps to a `CarrierId` through `data/carriers.toml`, by name or by the host of its `url` or `@id`, giving the ids of `insight-parcel` (`ups`, `usps`, `fedex`, `dhl_express`, `russian_post`), so the runner's carrier + number key meets the heuristic parcel. An unknown carrier gets an ASCII slug of its name (at most 32 chars); a name with no ASCII letter or digit drops the item. `track_url` is always `None` and `check_digit` `false`; `trackingUrl` becomes a separate `Track` action. When the runner merges it with a heuristic parcel, the kept item takes the heuristic `check_digit` and `track_url` (§6.1).

**Required fields and merge keys:**

| Type | Required | Merge key | Merged into one item |
|---|---|---|---|
| Flight | `reservationNumber` or `flightNumber`; `departureAirport.iataCode`, `arrivalAirport.iataCode`, `departureTime` | carrier + flight number + local departure date; without a flight number, reservation number + departure airport + date | passengers and seats (lists, at most 9 each) |
| Lodging | `reservationNumber`, `reservationFor.name`, `checkinTime` (or `checkinDate`) | reservation number + check-in date | guests (at most 9) |
| Train, Bus | `reservationNumber` or `trainNumber`/`busNumber`; departure station; `departureTime` | number (else reservation number) + departure station + local departure date | passengers, seats |
| Event | `reservationFor.name`, `reservationFor.startDate` | name + start | tickets |
| Food | `reservationFor.name`, `startTime` | name + start | party size |
| RentalCar | `reservationNumber`, `pickupTime` | reservation number + pickup date | |
| Order | `orderNumber` or `seller.name` | order number, else seller | items count |
| Invoice | `totalPaymentDue` or `paymentDueDate` | number, else payee (the runner: neither gives no key; inside schemaorg: total + due date) | |
| ParcelDelivery | `trackingNumber` and `carrier` | carrier + number | |
| Pass (pkpass) | `organizationName`, `description` | attachment ordinal | |

A four-passenger round trip is therefore two Flight items. Lists are joined only inside this extractor; the runner joins items of one fact across extractors by the same keys (§6.1).

**Rules:**

- **Confidence:** `Certain` once the required fields validate. Otherwise the item is dropped. Unknown types are ignored.
- **Forged markup** is extracted and labelled with the message trust, never dropped and never acted on.
- **Hard negatives:** depth and graph bombs (`@id` cycles, `itemref` cycles and fan-in), an end before the start, `@type` arrays mixing unknown types (only the known members are kept), and URLs with refused schemes.

- **Field shapes:** airline codes are 2 ASCII letters or digits and airports 3 letters, both uppercased; a flight number loses its airline prefix; seats and tickets must fit as identifiers; passengers, guests and tickets are capped at 9.
- **Money:** a JSON float price is read through its shortest decimal text, so `1e3` in a JSON number is accepted as `1000`; an exponent in a string price is rejected (§4.1).
- **Explain** gives rule names, block or item positions, counts and snake_case type keys only, never values, hosts or URLs.

**Times.**

- `…+01:00`, `…Z`, `…+0100` and `…+01` are `Zoned` in the written offset with no zone name. Without an offset the time is `Local`. A bare date is `Local` at 00:00. Seconds and fractions are optional; anything else is missing.
- An end before the start compares two zoned times as instants and any other pair as wall times.
- Zoned times are shown in their own offset with the zone abbreviation, never converted. Local times are shown as written.
- An `EventReservation` is hidden when an invitation resolves to the same instant within ±1 minute. A Local time is compared as wall time on the invitation's TZID.

### 7.6 Passes (`insight-pass`, Phase 37)

- Only `pass.json` is read: exactly one style key (none or several give nothing), `organizationName` and `description` (both required), `relevantDate` and the top-level `semantics`. `barcodes[].message` is only counted for `explain`. Field arrays and field-level semantics are never read: their keys are the issuer's choice.
- `relevantDate` with an offset is `Zoned` (no zone name), without one `Local`; a date with no time gives `None`.
- `boarding` is filled from `semantics` only, for a `boardingPass` with `transitType` `PKTransitTypeAir` and both airports (`departureAirportCode`, `destinationAirportCode`). It holds one leg: airline code (2 letters or digits; ICAO codes are refused), flight from `flightNumber` else `flightCode`, date from `currentDepartureDate` else `originalDepartureDate`, the first seat, the PNR from `confirmationNumber` and the passenger name. Without both airports `boarding` is `None`; there is no fallback to the field arrays.
- Archive caps (§6.1) are checked from the central directory before anything is inflated, and `pass.json` is read through a counting reader. Archives with repeated entry names, absolute or `..` paths or NUL in a name are refused. Only `pass.json` is ever inflated.
- `signature_checked` is always `false` in this plan; `manifest.json` and `signature` are not read.
- **Later (with `holtmail-barcode`):** `holtmail-bcbp` parses BCBP strings into `BoardingPass`.
  - The year resolves to the candidate date nearest to `sent_at − 2 days`, going forward and within `[sent_at − 30 d, sent_at + 330 d]`. Day 366 is handled.
  - When `sent_at` is missing, the date stays day and month only.
  - The fixtures `bcbp-utc-date-boundary.eml` and `bcbp-no-date-header.eml` belong to that work.

## 8. Data file

**Builder** (`apps/category-builder`, binary `holtmail-category-builder`, on `holtmail-category-data`):

- **Wikidata.**
  - `data/industries.toml` (CC0, checked in) maps root QIDs to topics. Subclasses are expanded with one `wdt:P279*` query per root.
  - Organisations are fetched in `VALUES` batches of 20–50 QIDs, one request at a time.
  - User-Agent: `holtmail-category-builder/<v> (https://github.com/holtmail/holtmail) reqwest/<v>`.
  - Backoff on 429 honours `Retry-After`.
  - A response counts only if it parses as complete SPARQL JSON, because the endpoint answers 200 with a truncated body and a stack trace. The build fails rather than shipping a partial file.
  - A second pass on P31 roots covers organisations without P452 (airlines, hospitals, pharmacies). The QIDs are verified before use.
  - Only a site root becomes a key: a bare or `www` host is keyed by its registrable domain, any other subdomain by its full host; a website with a path is skipped. Hosts at or under `skip_domains` (site builders, shorteners) or `mailbox_domains` (consumer mailbox providers and ISPs) in `industries.toml` are never written, and `golden.toml` `absent` entries fail the build if a row lands on them.
- **Sensitive rows are reviewed.** A row may carry `finance`, `health` or `government` only if one of these holds:
  - it was in the previous release with the same topic;
  - its domain is in `data/sensitive-reviewed.toml`, a maintainer-approved diff checked into the repository.

  New or changed sensitive rows that are not reviewed are written without the sensitive topic and listed in the build report. The gate fails when more than 50 such rows appear in one build, which forces a review.

  `--first-release`, for the first snapshot only (no previous file), strips unreviewed sensitive topics without failing the gate. Later builds with `--previous` count every sensitive topic it stripped as unreviewed, so the next snapshot needs the review first. `check` fails on any unreviewed sensitive row.
- **UT1** comes from `olbat/ut1-blacklists` at a pinned commit. The official site has been down since October 2026. It is **non-sensitive only**:

  | UT1 category | Topic |
  |---|---|
  | `shopping` | `purchases` |
  | `jobsearch` | `jobs` |
  | `social_networks` | `social` |
  | `bank`, `financial` | nothing |

- **Tests:** the unit tests run on recorded SPARQL and UT1 fixtures inside `mise run check`. Network code runs only in CI.

**Format** (`holtmail-category-data`):

- A sorted TSV `registrable_domain\ttopic[,topic]\tsource`, gzip-compressed, with the header `#holtmail-categories 1 <name> <version>`.
- Loading caps the decompressed size at 32 MiB and the rows at 500k, and refuses unsorted or duplicate keys.
- Topic names this build does not know are skipped on load, and a row left with no topic is dropped.
- It loads into a sorted `Vec<(Box<str>, TopicSet)>` with binary search: about 132k rows, about 6 MB in memory.
- Lookup tries the exact host, then its registrable domain.

**Licensing.**

| File | Contents | License |
|---|---|---|
| `wikidata-categories.tsv.gz` | Wikidata plus Holtmail curation | CC0 |
| `ut1-categories.tsv.gz` | the mapped UT1 categories | CC BY-SA 4.0, with `LICENSE-DATA` and UT1 attribution |

BY-SA data is never bundled into the binary and never committed to the repository.

**Distribution (Phase 42).**

- `.github/workflows/categories.yml` runs weekly, in four jobs, every action pinned to a commit:
  - **`build`** has no secrets. It fetches the data, runs the builder and the sanity gate, and uploads the files as a workflow artifact.
  - **`manifest`** has no secrets and never contacts the sources. It builds the builder from source, downloads the artifact and, itself, the previous release, re-runs the sanity gate and writes the unsigned `manifest.json` (`holtmail-category-builder manifest` with `--key-id` from the repository variable `CATEGORIES_KEY_ID` and `--not-after-days 60`).
  - **`sign`** has only `CATEGORIES_SIGNING_KEY` (in the env of one step) and `contents: read`. It compiles nothing and runs no third-party action: `scripts/sign-manifest.sh` checks the manifest against the files and signs it with OpenSSL.
  - **`publish`** has `contents: write` and no secret. It publishes on the tag `categories-YYYYMMDD` and on the rolling release `categories-latest` (sequential uploads: data files first, then the signature, then the manifest), then downloads both and compares every asset with the signed files. The workflow runs every Monday at 03:23 UTC and on `workflow_dispatch`; UT1 comes from the pinned commit `03501c64d230df9d84124176558d758351027be1`.
- **`manifest.json`:** `{format: 1, version, key_id, not_after, files: [{name, sha256, size, license, rows}]}`, at most 64 KiB, unknown fields refused. `version` is 1–16 ASCII digits compared as a number (the workflow uses `YYYYMMDDHH`, above the bundled `2026110701`); `key_id` is `[a-z0-9-]{1,32}`; `not_after` is RFC 3339 UTC in exactly the form `YYYY-MM-DDTHH:MM:SSZ`; `files` holds one or two distinct entries with a lowercase hex `sha256`, a `size` of 1 byte to 16 MiB, the license fixed for that file name (`CC0-1.0`, `CC-BY-SA-4.0`) and `rows` (data lines without the header, at most 500k). `manifest.json.sig` is the 64 raw bytes of an ed25519 signature (`verify_strict`) over the manifest bytes.
- **Keys.** `category-domains` compiles in a current and a next public key (`CURRENT_KEY`, `NEXT_KEY`), selected by `key_id`. Both slots are empty until the maintainer generates the production pair (VERIFY), so every release is refused with `no_release_key` until then. The `test-key` feature, forwarded from `insights-test-data`, adds the key `test`, whose seed is a committed fixture. Manifests past `not_after`, or with `not_after` more than 60 days ahead, are refused. Rotation ships the next key in a release before it signs anything (the steps are in the `category-domains` README).
- **File names** are accepted only from the fixed set `{wikidata-categories.tsv.gz, ut1-categories.tsv.gz}`.
- **Sanity gate.** It fails when:
  - the row count of a file changes by more than 20%;
  - any entry in `data/golden.toml` (stable organisation domains with known topics) changes topic;
  - more than 50 unreviewed sensitive rows were added or changed;
  - the manifest is not monotonic.
- **Offline by default.** The CC0 file is `include_bytes!`'d from `crates/category-domains/data/wikidata-categories.tsv.gz`, at most 2 MiB. It is refreshed per release by `mise run categories-snapshot`, with a reviewed diff. The first snapshot is produced in Phase 39 by running the builder by hand.
- **Updates are opt-in:** `[insights.categories] update = false` by default, and `data_sources = ["wikidata"]` names the downloaded files to fetch (adding `"ut1"` is a separate opt-in; §17.10). An unknown or repeated name is a config error, and so are `update = true` without `domains` in `sources` or with an empty `data_sources`. The bundled Wikidata table always loads, whatever `data_sources` says. When updates are enabled, a loop checks after `max(300 s, last check + 168 h − now)`, so a restart does not shorten the interval, then every 168 h; checks never overlap:
  - It calls `DataDownload::get(url, etag, max_bytes)` for `manifest.json` (64 KiB cap), `manifest.json.sig` (1 KiB) and each listed file named in `data_sources` (capped at its manifest `size`). The request is https only, connects directly like `json-http` (no proxy setting exists for it), sends no cookie, `Authorization` or `Referer`, follows at most 2 redirects and contacts only the pinned hosts `github.com`, `objects.githubusercontent.com` and `release-assets.githubusercontent.com` (exact names, no subdomains), and has a 60 s timeout. `tls_roots` is honoured. A 304 is `NotModified`, any other non-2xx status is unavailable, and a body over the cap is refused.
  - The base URL (default `https://github.com/holtmail/holtmail/releases/download/categories-latest/`), the pinned hosts, the interval and the first delay are configurable only under the `insights-test-data` feature (`update_base_url`, `update_hosts`, `update_interval_secs`, `update_delay_secs`); a release build refuses those keys as unknown.
  - It sends `If-None-Match` on every hop (the asset host answers the 304) while a stored release is loaded, verifies the signature, the key id and the expiry, and refuses a version that is not newer than the loaded one; the same version with a new ETag counts as not modified. When no stored release loads, it sends no ETag and refuses only a version older than `categories_data_version`, so a damaged release of the same version is repaired. It then checks each file's name, size, sha256, header table name and version, and row count.
  - It writes to `{data_dir}/categories/` in this order: the data files, `manifest.json`, `manifest.json.sig`, each through a temporary file in that folder, fsynced and renamed, then the folder fsynced. Settings keep `categories_data_etag`, `categories_data_version`, `categories_data_checked_at` (Unix ms) and `categories_data_outcome` (`updated | not_modified | refused | failed`). A refused or failed check changes neither the files nor the loaded tables nor `derive_fp`. A download the fetcher refuses (unpinned host, too large) counts as `refused` with the reason `download_refused`.
  - Logs carry versions, file names, sizes, row counts, reason codes and timings, never rows, hashes, URLs or key material.
- **Start-up.** The stored release loads only if `verify_stored` accepts it: the signature and a compiled-in key, without the expiry, so a stored release keeps working after `not_after` until a newer one arrives. Each file named in `data_sources` must then pass `check_file`; anything that fails is ignored with a warning and the bundled table stays. A crash between the file writes and the signature leaves a release that fails these checks, so start-up falls back and the next check downloads it again. A downloaded `wikidata` table replaces the bundled one only when its version is newer; otherwise the bundled table is kept, so a server upgrade with a newer snapshot wins over an older download.
- **Effect of a new file.**
  - `apps/server` holds the concrete `Arc<DomainsCategorizer>` and calls `reload`. The runner sees only `dyn Categorizer`.
  - The new snapshot changes `derive_fp`, and `WalkWake` runs the stage-B pass, which restarts on the version change (§6.5). **No blob is read.**
- **Settings → About** (a new page, Phase 42, open to every signed-in user) shows the Holtmail version, the loaded files' names, versions, row counts, licenses and whether each is bundled or downloaded, "Contains data from UT1 (Université Toulouse Capitole), CC BY-SA 4.0" when UT1 is loaded, and whether updates are on with the last check and its outcome. It reads `GET /api/insights/data` (§10).

## 9. Search

**Index fields.** `IndexDocument` gains:

- `insights: Vec<String>`: kind tags at `Likely` or higher, non-forwarded (`code`, `link`, `unsubscribe`, `reservation`, `order`, `invoice`, `parcel`, `pass`).
- `categories: Vec<String>`: effective values such as `kind:bulk` and `topic:travel`.

Both are tantivy `STRING` fields, added in **one** schema bump in Phase 40. **Values are never indexed.**

**Clauses.** `Clause::Insight(String)` and `Clause::Category(String)` are exact and case-insensitive, like `Label`. They have conformance cases in `holtmail_index::testing`. Item documents leave them empty.

**Syntax:**

- `category:<topic or kind>` (`category:travel`, `category:bulk`). An unknown value is the new `QueryError::UnknownValue`. Nothing could depend on a new operator name.
- `has:code|link|unsubscribe|reservation|order|invoice|parcel|pass`
- `is:newsletter` (kind `bulk`), `is:notification`, `is:personal`, `is:list`
- Unknown `has:` and `is:` values keep falling back to text.
- The web autocomplete list `operators` in `apps/web/src/lib/query.ts` gains `category:`, the new `has:` values and the new `is:` values.

**Meaning change.**

- These strings parse as free text today (`crates/query/src/lib.rs:137-146`).
- Deletion queries are one-off, in-memory reviews (`apps/server/src/deletion.rs:15-30`), so there are no stored queries to check. The release note lists the operators, and a review typed with `has:code` now selects differently. The review's count and sample show this before confirmation.
- A deletion review whose query uses `category:`, `is:` or `has:` with a new value, negated or not (`Query::reads_insights`), is refused with `MailboxError::Conflict`, a 409 "Some messages are still being read. Try again when search has caught up.", while the archive has pending insights, categories rows behind the archive's current `derive_fp` (`count_needing_derive`), a non-empty `insight_reindex` (`count_reindex_queue`) or `index_rebuild` is set. The refusal also signals `WalkWake`. It never acts on a lagging index. With insights off such a review is refused with `MailboxError::Invalid` (400): rows written while insights were on stay in the store and keep reaching the index, but nothing keeps them current.

**Counts.**

- `category_counts` is a SQL `GROUP BY` over `message_topics_filter` and `message_categories_kind`. It is never computed from the index.
- `/categories` leaves out `health` unless `?include=health` is passed. The reader's "Show health" link passes it.

**Backfill.** `GET /api/archives/{id}/insights/status` returns `{pending, running, reindex, categorize}` (`InsightsStatusDto.reindex` is `count_reindex_queue`, `categorize` is `count_needing_derive` under the archive's current `derive_fp`). While `pending + categorize + reindex` is above 0, search and the sidebar show "Some messages are still being read (N left)" with N that sum, polled every 5 s.

## 10. API and access

**`MessageDetail.insights: Option<MessageInsightsDto>`.** `None` means an older server.

- `status`: `ready | pending | outdated | partial | unparsable | unreadable`.
- `trust`: `{state, domain?, provider?}`.
- `own`: bool.
- `kind`: `{id, source, overridden}`.
- `topics`: `[{id, source, overridden, held_back?}]`. `held_back: "not_verified"` marks an override topic the trust gate kept off.
- `items`: `Vec<InsightDto>`, a tagged union with `confidence`, `score`, `forwarded` and, for actions, `display_host`, `via`, `tracked`, `text_domain_mismatch`, `destination_differs_from_sender` and `confusable`. It holds `Likely` and higher, plus `Possible` items when `show_possible` allows them.
  - `reservation` carries a `ReservationDto` tagged by `kind` (`flight`, `lodging`, `train`, `bus`, `event`, `food`, `rental_car`); train and bus numbers are both `number`. `order` has `number`, `merchant`, `total`, `status`; `invoice` has `number`, `payee`, `total`, `due` (`YYYY-MM-DD`). `pass` has `style` (`boarding_pass | event_ticket | coupon | store_card | generic`), `organization`, `description`, `relevant`, `boarding` (`pnr`, `passenger`, `legs` with `from`, `to`, `carrier`, `flight`, `date`, `seat`), `attachment` and `signature_checked`.
  - Every URL on these items comes with `url_host`, the `display_host` of its host (an A-label when confusable). A URL without a host (`mailto:`) gives `url` and `url_host` both `null`.
  - `TimeDto` is `{local: "YYYY-MM-DDTHH:MM:SS", offset: "+01:00" | null, zone: string | null}`. `offset` is `null` for a Local time; fractional seconds are dropped. Nothing is converted.
  - `MoneyDto` is `{amount, currency, exponent}`, where `amount` is a decimal string with exactly `exponent` fraction digits (`"1234.50"`, `"-5"`, up to `i64` range). It is never a JSON number.
  - A pass date is either `"YYYY-MM-DD"` or `{month, day}`.
- `unsubscribe`: `{https, mailto, http, one_click: {available, reason?}, last_request?}`. `last_request` is `{outcome, status, requested_at}` (`outcome` one of `accepted | redirected | refused | http_error | network`, `status` the HTTP status or `null`, `requested_at` in Unix ms): the newest request for the item's list key. It is left out when there is none, and always for members. `reason` is one of `no_post_header | not_https | multiple_fields | not_covered | not_aligned | no_trusted_authserv | disabled | recently_requested | members`. The first that applies is given, in this order:
  1. `members`;
  2. `no_post_header`: no `List-Unsubscribe-Post` field;
  3. `not_https`: no https link on port 443 (explicit or default) with a host and without a user name or password;
  4. `multiple_fields`: more than one `List-Unsubscribe` or `List-Unsubscribe-Post` field in the auth facts;
  5. `no_post_header`: the field is not exactly `List-Unsubscribe=One-Click`;
  6. `no_trusted_authserv`: trust `unknown`, or no stage-B row yet;
  7. `not_aligned`;
  8. `not_covered`;
  9. `disabled`: `[insights] one_click = false` (the kill switch);
  10. `recently_requested`: the newest request for the same list key was `accepted` or `redirected` less than 24 h ago.

  Otherwise `available` is true and `reason` is absent. A failed request does not change the reader; only the route answers 429 for the next minute.

It is read with `insight_view`: zero blob reads, and at most 4 indexed row reads in one store call, plus, for an owner or admin, one indexed `last_unsubscribe` read when an unsubscribe item has a list key. `get_thread` uses `insight_views_among` and `last_unsubscribes` once per thread.

**Member filtering**, done server-side for members (not owners or admins):

- `Code` items are removed.
- `Action` items with `sign_in`, `magic_link`, `reset_password`, `verify`, `confirm` or `activate` are removed.
- `unsubscribe` keeps only `{one_click: {available: false, reason: "members"}}`, with no links. A GET on many https unsubscribe links already unsubscribes.

**List rows.** `MessageSummary` gains `insight_kinds: Vec<String>`, `kind: Option<String>` and `topic: Option<String>` (`list_topic`, never `health`). It never carries values. For members, `code` is left out of `insight_kinds`; `link` stays, because the stored bits cannot tell sign-in actions from the ones members see. On own mail, `code` and `link` are left out. The list follows the stored bit order: `code`, `link`, `unsubscribe`, `reservation`, `order`, `invoice`, `parcel`, `pass`. Members and own mail keep every reservation, order, invoice and pass item.

An action whose destination has no host is dropped from the DTO, so `display_host` is always present; the extractors never store one.

**Routes.** All of them are under `/api/archives/{id}/…` or `/api/messages/{id}/…`, so `access.rs` checks grants.

| Route | Members |
|---|---|
| `GET /api/archives/{id}/categories` (taxonomy, counts; `include=health`) | read |
| `GET /api/archives/{id}/insights/status` | read |
| `GET /api/archives/{id}/messages?category=` | read |
| `GET /api/archives/{id}/category-overrides` | read |
| `PUT /api/archives/{id}/category-overrides` `{scope, key, kind?, topics?}`; idempotent on `(scope, key)`; 400 "nothing to change" | 403 |
| `DELETE /api/archives/{id}/category-overrides/{override}` | 403 |
| `POST /api/messages/{id}/unsubscribe` `{"confirm": true}` | 403 |
| `GET /api/insights/data` (loaded files, versions, licenses, update state; not under an archive) | read; added to the `access.rs` match explicitly, a POST is 403 |

- Members get 403 on writes through `access.rs:64`.
- `/categories` answers `{kinds: [{id, count}], topics: [{id, count}]}` in taxonomy order with zeros. Any `include` value other than `health` is 400.
- `?category=` takes a kind or topic name; an unknown one is 400. `category=health` while health is off gives an empty page, because rows derived before the flag changed may still hold it.
- The category and override routes answer 404 when insights are off, as `/insights/status` does.
- `GET /api/insights/data` answers 200 `InsightsDataDto {holtmail_version, files: [DataFileDto], updates: DataUpdatesDto}` to every signed-in user. `DataFileDto` is `{name, file, version, rows, license, origin: bundled | downloaded, attribution}` (`attribution` is the UT1 line for `ut1`, else `null`); `DataUpdatesDto` is `{enabled, data_sources, last_checked_at (Unix ms or null), last_outcome: updated | not_modified | refused | failed | null}`. It reflects the tables the categorizer votes with at the moment of the request. With insights off, or without `domains` in `sources`, it answers 200 with no files and updates disabled. Its OpenAPI entry lists 200, 401 and 500.
- Override DTOs leave out unknown topic bits, and `health` while health is off. A replacing PUT keeps a stored `health` bit while health is off.
- Every handler lists 401, 403, 404 and 500 in its OpenAPI entry.
- `members_read_only_granted_archives` and the integration `access.rs` matrix gain the new routes.
- API tests check that a member's `MessageDetail.insights` and `MessageSummary.insight_kinds` follow the filtering above.

**Account trust lists.** The existing account update (`PATCH /api/accounts/{id}`) gains `trusted_authserv_ids` (owner or admin). Changing it marks that archive's stage-B rows stale through `derive_fp`. `AccountDto` gains `trusted_authserv_ids` and `suggested_authserv_id` (§4.3); both are `null` for members and for import accounts, SMTP included, whose lists come from config, and editing an import account's list is 400.

**One-click unsubscribe:**

- **Who:** owner or admin only.
- **Checks.** The server **ignores any URL from the client**:
  - When the stored insights row's fingerprint is not the current `extract_fp`, the server first recomputes that message (`Ingest::refresh_insights`, one blob read and parse). Whenever the stage-B row is missing or its `derive_fp` differs from the current one (a trust-list edit, an alias change, a PSL bump), it recomputes that row in place with fresh snapshots before reading `one_click_ok`.
  - It then re-checks: `one_click && one_click_ok`; https; port 443 (explicit or default); no userinfo; `[insights] one_click = true` (the kill switch).
  - An earlier request for the same `list_key` whose outcome was `accepted` or `redirected` blocks a new one for 24 h, and its stored outcome is returned. After `refused`, `http_error` or `network`, a retry is allowed after 1 minute.
  - Re-reading the row, the checks, posting and recording run under one process-wide lock, so two concurrent confirmations never send twice; the recompute before it runs outside the lock.
- **Answers** (request body `{"confirm": true}`):
  - 200 `UnsubscribeResultDto {request: {outcome, status, requested_at}, repeated}`. `repeated: true` means nothing was sent and the stored request of the last 24 h is returned.
  - 400 `UnsubscribeRefusalDto {error, reason}` with the first one-click `reason` that applies, in the order above; `reason` is `null` when the body does not confirm.
  - 429 with `Retry-After` in seconds when a request for the same list failed less than a minute ago.
  - 401, 403 for members, 404 for an unknown message or with insights off, 500.
- **The request:** `OneClick::post(url)` in `fetch-http`:
  - It never uses a proxy. A `socks5h` or `http` proxy would resolve the name itself, and `is_public` could not be enforced.
  - It resolves every A and AAAA record and refuses when **any** of them is not public. It then pins the connection to the checked addresses with reqwest `resolve_to_addrs`, so TLS still verifies the original hostname (SNI and certificate).
  - No redirects: a 3xx is recorded as `redirected`.
  - No cookies, auth or referrer.
  - The body is `List-Unsubscribe=One-Click` as `multipart/form-data`.
  - 10 s timeout over the whole call, name resolution included. The response body is discarded after 64 KiB.
  - Outcomes: `accepted` for a 2xx, `redirected` for a 3xx, `http_error` for any other status, `refused` when nothing was contacted (a scheme, port, userinfo or address check failed), `network` for DNS, connect, TLS or timeout failures.
  - Its resolver is the only path to DNS (the client's own resolver always fails), and it never connects to an address it did not check.
- **Recording:** the outcome is stored in `unsubscribe_requests` and returned. `[insights] one_click` defaults to true; with it false the server builds no `OneClick` and every message reports `disabled`. One-click always connects directly, whatever `image_proxy` says.
- **Logs:** archive, message and user ids, the outcome and the HTTP status. Never the URL, domain or list id.
- **Mailto unsubscribes** are never sent by the server. The archiver sends no mail, so those links open as normal navigations.

**Webhooks.** `task.finished` with kind `analyse` (ids and counts only) is allowed. The `Event::TaskFinished` doc (`crates/hooks/src/lib.rs:64-70`) and the `api-client` README list the new kind. No other insight event exists.

After every API change, `openapi.json` and `schema.ts` are regenerated and the `api-client` README is updated.

## 11. Web

**Card** (`apps/web/src/mail/insights-card.tsx`):

- It sits in `ThreadItem` after the address `<dl>` and before `<Invitations>` (`thread-item.tsx:222`). It renders only for expanded messages.
- It is a hairline box on `bg-bg-subtle`, square. It has its own `InsightRow`, a shared row component extracted with `Row` from `invitation-card.tsx:124-131` into `mail/row.tsx`.
- It returns `null` when nothing is visible.
- Rows are focusable (`tabIndex=0`). Arrow keys move between them, and `Enter` or `c` act on the focused row.
- The order comes from `lib/insights.ts::orderInsights`:
  1. **Code.**
     - Shown in `font-mono tabular-nums` and copied without spaces, with a `CopyButton`.
     - Display grouping by length: 4, 5 and 7 digits ungrouped; 6 digits as 3+3 (`482 913`); 8 digits as 4+4; alphanumerics ungrouped.
     - The row collapses to "Code from 3 days ago" with Show when `now − t > 15 min`. `t` is the earlier of `received_at` and `sent_at` when both exist, otherwise whichever exists. Now is the client clock, re-evaluated every 30 s. With no time known, the row is collapsed.
     - Copy still works from the collapsed row.
  2. **Primary action.**
     - Only one action row: the sender's own before forwarded ones, then the highest confidence, then the highest score. A second `Likely` action from the API is not shown.
     - Shown as "Reset password → example.com", with `display_host` in mono and "via outlook.com" when unwrapped.
     - Badges, all outline: "Tracked"; warning "Link text names another site" (`text_domain_mismatch`); warning "Goes to another site than the sender" (`destination_differs_from_sender`); warning "Look-alike address" (`confusable`, A-label shown); warning "Sender not verified" on `unverified` and `relayed` mail.
     - It is a plain `<a target="_blank" rel="noopener noreferrer" referrerpolicy="no-referrer">`.
  3. **Reservation or pass.** Reservations come before passes.
     - One compact row each, for example `LH 1234 · FRA → LIS · 12 Mar 07:15 CET · Seat 14C · Ref ABC123`, with a copy button on the reference and "Open → host" for the item's URL. A stay without check-out shows "Check-in 12 Mar 15:00".
     - Times are shown in their own offset (§7.5), parsed from the DTO string, so the viewer's time zone is never used. The zone abbreviation (`en-US`, then `en-GB`) is shown only when the zone's real offset at that instant equals the written one; otherwise "UTC+05:30", or "UTC" for zero. A Local time shows nothing after the clock time. The year is shown only when it differs from the message's year (the local year of the earlier of `received_at` and `sent_at`); a pass date of month and day never shows one.
     - An `EventReservation` is hidden when an invitation on the message starts within ±60 s, inclusive. A zoned time compares as an instant with the invitation's `utc`; a Local time compares as wall time on the invitation's TZID, or with its wall time when it has none or is floating. All-day invitations and invitations without a start never match.
     - Forwarded items, of every kind, carry "Forwarded".
     - `Enter` on a reservation row opens its link, else copies the reference; on a pass row it copies the PNR.
  4. **Parcel.** "1Z… [copy] Track → ups.com".
  5. **Order or invoice.** Orders before invoices. Number, merchant or payee, total, status and due date from markup, with "Open → host". Money is formatted with `Intl.NumberFormat('en')` from the decimal string with exactly the currency's exponent, never through a float; an amount that is not a plain decimal or an invalid currency falls back to `amount currency`.
  6. **Footer.**
     - Kind and topic chips (`Badge variant="outline"`, linking to `?category=`).
     - Unsubscribe (`mail/unsubscribe-control.tsx`): when one-click is offered, a button opens a `Dialog`: "Ask example.com to stop sending to you? Holtmail will contact their server once, directly." The domain is the host of the stored https link (lowercase, no port or userinfo, an A-label kept), else the aligned domain, else the sender's. Confirm is disabled while the request runs; the outcome then shows in the dialog (a `repeated` answer starts "Already requested on \<date\>.", a 429 says to try again in a minute), with "Open their unsubscribe page" for every outcome but `accepted`. The dialog stays open while `['messages', id]` and `['threads']` refetch. Otherwise the https link or the mailto, with the `reason` in a `Tooltip`. After a request, "Unsubscribe requested on \<date\>" (`formatFullDate`, with the outcome when it was not `accepted`).
     - "Sender not verified", "via SimpleLogin, original sender not verified", or a muted "Sender not checked" for `unknown`.
  - `partial` and `outdated` rows render the same way as `ready`. `pending`, `unparsable` and `unreadable` render nothing.
- **Copy:**
  - `@holtmail/ui` gains `CopyButton` and `useCopy`. They replace the copies in `reader.tsx:207-231` and `integrity-page.tsx:16-33` (the second one has no `catch`).
  - The fallback chain is the pure, tested helper `copyText(env)`: `navigator.clipboard.writeText`, then `execCommand('copy')` on a hidden textarea (works over plain HTTP), then select the value in a read-only `Input` with "Press Ctrl+C" / "⌘C".
  - `aria-live` announces "Copied" only.
- **Keyboard.** Each key gets entries in `lib/keymap.ts`, `shortcutList` and `strings.shortcuts.items`. The target is the focused `ThreadItem`, else the newest expanded message. Its insights reach `mail-layout` through a ref in `mail/context.ts`.

  | Key | Action |
  |---|---|
  | `c` | Copy the top value: code, else the first visible reservation's booking reference, else a pass PNR, else tracking number; on a focused card row, copy that row |
  | `Shift+L` | Open the primary action link (the tracking link when there is none) |
  | `Shift+U` | Unsubscribe: opens the dialog when one-click is offered, otherwise opens the https link |
  | `x` | Change the category: opens the chip menu |
  | `Enter` on a card row | The row's action: reveal a collapsed code, open its link, or copy |

  All of these keys are free in mail (`lib/keymap.ts`) and in the planned Chats keymap (CONVERSATIONS §11.5, where `Shift+O`, `o`, `r`, `v`, `d` and `n` are taken). Keys do not reach the app while focus is inside the sandboxed message iframe. This is an accepted limitation: `Esc` or a click outside returns focus.
- **List.** A kind glyph for `bulk` and `notification`, a code glyph for `has code`, and at most one topic chip (`topic` from the DTO) in the existing chip style. There is never a health chip and never a value.
- **Sidebar.** "Categories" shows kind and topic counts from `/categories` (without health) and links to `?category=` (a SQL filter). The palette gains "Show travel" and similar entries.
- **Overrides.**
  - The chip menu offers kinds and topics, then "Apply to: ada@example.com / everyone at example.com". It shows "Not applied here: sender not verified" for held-back topics.
  - It applies optimistically and invalidates `['archives', id, 'messages']`, `['archives', id, 'categories']` and `['messages', id]`.
  - **Settings → Categories** is a new page (route in `settings-layout.tsx`, apps/web docs). It lists the overrides with Delete.
- **Settings → About** (Phase 42, `/settings/about`, last in the settings nav, for every signed-in user) reads `insightsDataQuery()` and shows the Holtmail version, the loaded data files as a hairline list (name with a bundled or downloaded badge, version, rows, license, file name), the attribution lines (the UT1 line whenever a `ut1` file is loaded, from `strings.about` when the server sends none), and the update state in muted text. With no files loaded it shows a note in a bordered box. `lib/about.ts` (`dataFileRows`, `attributions`, `updateLines`) holds the pure text and has 11 tests.
- **Account settings.** The account editor gains "Trusted authentication servers" (`trusted_authserv_ids`), prefilled from presets.
- **Strings.** `strings.insights.*`, `strings.categories.kinds`, `strings.categories.topics` (with a raw-id fallback) and `strings.unsubscribe.*`.
- **Tests.**
  - `lib/unsubscribe.test.ts`: the dialog domain and its fallbacks, each outcome's text and link offer, the 400 reasons and the 429, the requested-on text, and what `Shift+U` opens.
  - `lib/insights.test.ts`: ordering, the primary pick, the grouping table, copy text, the freshness rule (both timestamps, one, none), the reason text, the invitation dedupe, and the target pick for a thread of stale codes plus one fresh code.
  - `lib/categories.test.ts`: labels, override scope text, health excluded from chips and counts, held-back text.
  - `packages/ui` `copy.test.ts`: the fallback chain with stubbed APIs.

## 12. Privacy and logging

- Values (codes, URLs, numbers, names, totals) exist only in `message_insights.items` and in `MessageDetail.insights`. They are kept out of:
  - the index, list payloads and `MessageExtras`;
  - settings and the ledger;
  - webhooks (no insight event exists; `analyse` task events carry ids and counts);
  - tracing fields, including panic output (§6.1);
  - `InsightError`'s `Display`.
- Logs carry message and archive ids, extractor ids, versions, counts and timings.
- Codes are stored at rest. They are already in the blob and the preview, and storing them keeps the reader free of blob reads. Members never receive codes or sign-in links (§10).
- **Health** is computed locally like any topic.
  - It never appears in list chips, list payloads, sidebar counts (unless expanded), logs, webhooks or export manifests. It is visible only in the reader and in an explicit `category:health` filter.
  - `[insights.categories] health = false` turns it off. The flag is in `derive_fp`, so toggling it rewrites the stored rows and queues reindex.
  - A future LLM categorizer is accepted only for a loopback or private endpoint (`is_public` false), and it never receives messages whose derived topics include `health`.
- **`holtmail insights explain <message-id>`** prints to the operator's terminal only:
  - rule names, positions and scores;
  - candidates rendered through the shape skeleton (digits become `D`, letters `a`);
  - keywords only when they appear in `code-keywords.toml` or `link-actions.toml`;
  - markup tag names.

  It never prints values, hosts or URLs. A test checks that a fixture's code and host never appear in the output.
- The server never fetches a link from mail. The one-click POST is the only outbound request tied to mail content, and it needs consent each time.

## 13. Performance targets

| Measure | Target |
|---|---|
| Stage A, a message under 100 KiB | p95 ≤ 3 ms on the dev machine |
| Stage A at every cap (hostile corpus) | ≤ 40 ms, no allocation over 32 MiB (a test-only counting `GlobalAlloc` wrapper in the runner tests, no dependency). The release timing tests allow the larger of 40 ms and a multiple of the bare html5ever tokenizer on the same input (2× for the runner, 3× best of 5 for `html-scan`), since the tokenizer alone takes 20–48 ms on 2 MiB under load |
| Reader | zero blob reads for insights, one store call with at most 4 indexed row reads, plus one `last_unsubscribe` read |
| Row size | ≤ 16 KiB; about 0.5 KB average with auth facts |
| Walk | 2 parses at a time, ≥ 50 messages/s on SSD; 500k messages in about 3 h, resumable |
| Stage B pass | ≥ 20k rows/s; writer held ≤ 50 ms per batch of 500 |
| Override on a 50k-message domain | lists and counts exact in < 2 s; `category:` search within about 15 min of the wake |
| Counts at 500k messages | < 150 ms |
| Proof of one parse | a counting `BlobStore` test: one read per message with two walk steps registered, and zero reads in stage B, alias invalidation and override edits |

## 14. Failure modes

| Failure | Effect | Handling |
|---|---|---|
| Extractor panic | that extractor's items missing | `partial`; the hook logs only id, version, message id and location; retried when the fingerprint changes |
| Stored bytes no longer parse | no insights | `unparsable`, written by the caller; not retried until the fingerprint changes |
| Blob missing | no insights | `unreadable` with the current fingerprint; not re-listed |
| Write fails after insert | message without a row | the next sync's ingest (`no row` check) or the hourly anti-join |
| Walk killed | partial backfill | resumes from the anti-joins; no cursor |
| Extraction logic or a dependency changes | rows `outdated` | the golden snapshot forces a `VERSION` bump; old row served; the walk recomputes |
| Alias detection changes | trust and sender stale | `refresh_alias` invalidates stage B for the message |
| Data file, PSL, trust list or own addresses change | trust or categories stale | stage-B pass, SQL only; restarts on a mid-pass swap |
| Bad, unsigned, expired or unknown-key download | file refused | the previous file stays; logged by version only |
| Index behind categories | `category:` search lags | durable `insight_reindex` queue with `seq`; SQL filters exact; deletion reviews refused until drained |
| No trusted or learned authserv-id | mail is `unknown` | non-sensitive topics still apply; no warning badges; no one-click; no sensitive topics |
| Forged `Authentication-Results` | ignored unless it uses a trusted authserv-id | only trusted ids are read; the trusted MTA must strip forgeries of its own id (RFC 8601 §5); presets confirmed in VERIFY |
| DKIM `l=` or a prepended header field | possible injected content | `l=` gives `unverified`; extra fields break coverage |
| Stale row at one-click | wrong decision | recomputed before the checks |
| Sender redirects the one-click POST | not followed | `redirected` shown; the user can open the https link |
| Clipboard blocked (plain HTTP) | | `execCommand` fallback, then select and "Press Ctrl+C" |

## 15. Testing

**Conformance.**

- `holtmail_insight::testing::conformance::extractor(&dyn InsightExtractor, &[Case])` checks:
  - a stable `version()`;
  - empty output on plain personal mail;
  - two runs give equal bytes;
  - only allowed URL schemes;
  - the string and count caps, and that no identifier or URL is a prefix of a longer input value;
  - no panic and bounded output on the **shared hostile corpus**: 10 MB of one token, 5,000 nested tags, invalid UTF-8 boundaries, multi-byte text at every slice boundary, 1,000 anchors, JSON-LD at depth 500, an `@id` cycle, a microdata `itemref` cycle, an `itemref` fan-in bomb, a zip bomb, and a pkpass with `..` entries.
- `holtmail_category::testing::conformance::categorizer` covers the same properties, and that a snapshot's version and answers stay consistent.
- The store area `insights` covers:
  - every row state, fingerprints and the lenient `from_json`;
  - the changed-id results;
  - per-axis override precedence, key normalisation and the held-back sensitive topic;
  - counts with and without health;
  - an interleaved queue → read → queue → dequeue that leaves the row;
  - `invalidate_derived`;
  - both deletes.
- `holtmail_fetch::testing` gains `one_click_refusals` (refused schemes, userinfo, a non-443 port, private literals) and `one_click_cases`, which run against an in-process HTTPS server through a `OneClickHarness` (fixed resolver answers, addresses treated as public, the server's port, test roots, timeout): private addresses, mixed public and private answers, redirects not followed, 2xx versus other statuses, the multipart body with no cookie, auth or referrer, the timeout, a name lookup that never answers giving `Unavailable` within the timeout, an unknown host as `Unavailable`, and a certificate for another name failing TLS before any request. Five deliberately broken implementations each fail them.
- `holtmail_fetch::testing::data_download_cases` runs against the same in-process HTTPS server through a `DataDownloadHarness` (pinned hosts `data.example.com` and `assets.example.com`, unpinned `outside.example.org` and `mirror.data.example.com`, test roots, timeout): non-https and userinfo URLs, an unpinned first host or redirect target never contacted, a subdomain of a pinned host and IP literals refused, redirects up to 2 followed and the third refused, `If-None-Match` sent on every hop and 304 giving `NotModified` only when an etag was sent, other statuses `Unavailable`, the size cap from `Content-Length` and while reading (a body of exactly `max_bytes` accepted), no cookie, auth or referrer even after a `Set-Cookie` on a redirect, invalid or non-ASCII `ETag` dropped, and a stalled body timing out. Five deliberately broken implementations each fail them.
- `fetch-http` unit cases cover the new `is_public` ranges: `2002::/16` follows the embedded IPv4 address (6to4 of a public address stays public), `2001::/32` (Teredo) and `64:ff9b::/32` are always refused. With `--features test-networks`, a case proves only the listed networks are reached.

**Fixtures**, all invented, on `example.com` and `example.org`:

- `crates/auth-results/tests/fixtures/`: `ar-trusted-top.eml`, `ar-forged-top-untrusted.eml`, `ar-forged-trusted-below.eml`, `ar-comments.eml`, `dkim-l-tag-appended-link.eml`
- `crates/html-scan/tests/fixtures/`: one per rawtext and script element, `hidden-display-none-anchor.eml`, `hidden-template-code.eml`, `aria-hidden.eml`
- `crates/insight-code/tests/fixtures/`: `otp-plain-en.eml`, `otp-ru-html-strong.eml`, `otp-split-dash.eml`, `otp-wicg-origin-bound.eml`, `otp-near-order-number.eml`, `otp-price-date-phone.eml`, `otp-in-href-only.eml`, `otp-in-footer.eml`, `otp-hex-color.eml`, `otp-newsletter-year.eml`, `otp-tie-two-codes.eml`, `otp-zh-no-space.eml`, `otp-ja-fullwidth.eml`, `otp-ko.eml`, `otp-subject-and-body-same.eml` (one code), `otp-backup-codes.eml` (nothing), `otp-promo-code-newsletter.eml`, `otp-barcode-word.eml`, `otp-text-html-disagree.eml`, `otp-css-in-style-block.eml`, `fwd-inline-otp.eml`
- `crates/insight-unsubscribe/tests/fixtures/`: `unsub-one-click-covered.eml`, `unsub-one-click-h-missing-post.eml`, `unsub-one-click-other-selector.eml`, `unsub-one-click-unaligned.eml`, `unsub-prepended-second-field.eml` (not covered), `unsub-not-oversigned.eml` (covered), `unsub-bad-item-stops.eml`, `unsub-file-scheme.eml`
- `crates/insight-links/tests/fixtures/`: `link-reset-safelinks.eml`, `link-urldefense-v3.eml`, `link-lookalike-wrapper-host.eml`, `link-esp-tracked.eml`, `link-text-domain-mismatch.eml`, `link-destination-differs.eml`, `link-idn-homograph.eml`, `link-wallet-save.eml`, `link-unsubscribe-not-action.eml`, `link-forwarded-below-marker.eml`, `link-footer-only.eml`, `link-newsletter-shop-now.eml`; the runner's `tests/fixtures.rs` runs them through the full pipeline and checks `destination`, `via` and `tracked`
- `crates/insight-parcel/tests/fixtures/`: `parcel-s10-valid.eml`, `parcel-s10-bad-check.eml`, `parcel-ups-valid.eml`, `parcel-usps-valid.eml`, `parcel-fedex-12-valid.eml`, `parcel-fedex-15-valid.eml`, `parcel-dhl-express-valid.eml`, `parcel-russian-post-valid.eml`, `parcel-no-context.eml`, `parcel-phone-number.eml`, `parcel-forwarded.eml`
- `crates/insight-schemaorg/tests/fixtures/`: `jsonld-flight-graph.eml`, `jsonld-array-uri-type.eml`, `jsonld-multi-passenger-two-legs.eml` (two items), `microdata-lodging.eml`, `jsonld-forged-unverified.eml`, `jsonld-end-before-start.eml`, `jsonld-deep.eml`, `jsonld-id-cycle.eml`, `jsonld-price-jpy.eml`, `jsonld-price-kwd.eml`, `jsonld-price-overflow.eml`, `fwd-inline-booking.eml`, `jsonld-order.eml`, `jsonld-invoice.eml`, `jsonld-parcel-delivery.eml`, `jsonld-actions.eml`, `jsonld-unknown-type-mix.eml`
- `crates/html-scan/tests/fixtures/` (Phase 37): `microdata-lodging.html`, `microdata-itemref.html`, `microdata-itemref-cycle.html`, `microdata-hidden-div.html` (kept), `microdata-in-template.html` (nothing), `ld-json-below-forward.html`, plus a generated fan-in bomb and a nesting of 20 items
- `crates/insight-pass/tests/`: 22 archives built in-test from invented `pass.json`, among them a boarding pass, an event ticket, a lying and an honest zip bomb, `..` and absolute entries, 65 entries, a duplicate `pass.json`, encrypted and bzip2 entries; and a `.eml` with a base64 pkpass run through `holtmail_mime` and the runner
- `crates/category-signals/tests/fixtures/`: `kind-transactional-via-esp.eml`, `kind-list-discussion.eml`, `kind-newsletter-travel.eml`, `kind-unsub-only-transactional.eml`, `kind-auto-replied.eml`, `own-sent-order.eml`, `finance-spoofed-unverified.eml`, `finance-override-spoofed.eml`
- `crates/insights/tests/fixtures/`: `items-over-cap.eml`, `smtp-forged-ar-no-trust.eml`, `link-google-url.eml`

**Precision gate.**

- `crates/insights/tests/corpus/` holds at least 300 invented `.eml` files, at least 100 of them hard negatives. Each case has an entry in `labels.toml`:

  ```toml
  [[case]]
  file = "otp-plain-en.eml"
  code = "482913"
  links = [{ action = "sign_in", host = "example.com" }]
  parcel = [{ carrier = "ups", number = "1ZV77Y21K219503089" }]
  kind = "notification"
  topics = []
  ```

  `links` lists the expected `Likely` actions, primary first, with the destination host after unwrapping as a lowercase A-label. `parcel` lists the expected `Likely` parcels with the compact number. `structured` lists the shown reservations, orders, invoices, parcels and passes from JSON-LD, microdata or a pkpass as `{ type, ref }`, one per item (`ref` is the reservation, order, invoice or tracking number, else the train or bus number, name, merchant or payee; a pass by its organization). `negative` names the hard-negative family of a message (a code family, `link_*` or `parcel_*`). Since Phase 38 every case carries `kind` and `topics`, and the labels check refuses a sensitive topic. Since Phase 37 the corpus holds 364 messages, 156 of them hard negatives, with 22 expected structured items in 21 cases.
- It runs at the runner level, so extractor interplay is covered. The weights in the data files are what the gate tunes.
- `cargo test -p holtmail-insights --test corpus` fails below these thresholds and prints a confusion summary:

  | Kind | Precision | Recall |
  |---|---|---|
  | Codes | 0.98 | 0.85 |
  | Links (top-1) | 0.95 | printed |
  | Parcels | 0.99 | printed |
  | Unsubscribe | 1.0 | 1.0 |
  | Structured (multiset of type and reference) | 1.0 | 1.0 |
  | Kind (accuracy; the confusion matrix is printed) | 0.97 | |
  | Topics | 1.0 | printed |

  Links top-1: among messages where the runner yields at least one non-forwarded `Likely` action, the highest-scored one must match `links[0]` by action and destination host. Parcels: every non-forwarded `Likely` parcel must be in `parcel` by carrier and number.
- The golden snapshot (§6.1) runs over the same corpus.
- **Real false positives without real mail.** `holtmail insights explain` (§12) gives the maintainer a skeleton, and the maintainer writes an invented fixture from it. The skeleton is never committed.

**Property and fuzz.**

- `proptest` covers check digits, the RFC 2369 parser, `unwrap` idempotence and the identity of `unwrap` off the allow-list, `registrable_domain`, and the auth grammars.
- `cargo-fuzz` targets live in `crates/{html-scan,auth-results,links,insight-schemaorg,insight-pass,insights}/fuzz/`. Each `fuzz/` folder has its own `rust-toolchain.toml` pinning a dated nightly.
- The html-scan target includes the differential against `holtmail-sanitize`.
- Fuzzing runs nightly in `.github/workflows/fuzz.yml` (a cron at 02:17 UTC and `workflow_dispatch`), 10 minutes per target: `auth-results/parse`, `html-scan/scan`, `links/unwrap`, `insight-schemaorg/extract`, `insight-pass/extract` and `insights/extract` (seeded with the corpus). Each job installs its folder's pinned toolchain and `cargo-fuzz` 0.13.2, and uploads the `artifacts` folder on failure. Crashes become fixtures.
- Locally, `cargo +nightly-… fuzz` needs rustup's proxies; with Homebrew Rust on `PATH`, put the pinned toolchain's `bin` first (`PATH="$(rustup run nightly-2026-10-01 rustc --print sysroot)/bin:$PATH"`), as each crate's README shows.

**Server.**

- A counting blob store shows one read per message across the `insights` and `index` steps, and zero reads in stage B, alias invalidation and override edits.
- The walk resumes after cancellation, and `WalkWake` drains the queue at runtime.
- `panics_unwind` runs under `--release`.
- A panic-hook test runs an extractor that hits a char-boundary panic on a secret string, captures the log output and asserts the secret is absent.
- A stage-B test swaps a domain table mid-pass and asserts that no row carries a fingerprint it was not computed with.

**API.**

- Member 403 on every write, and member filtering of codes, sign-in links and unsubscribe links.
- 404 across archives.
- Unsubscribe refused when not covered, not aligned, not https, not port 443, disabled, or within 24 h of an accepted request (`repeated`, nothing sent). A retry within a minute of a failure is 429, after it is allowed. Captured logs carry no host, path, list id or client URL.
- A stale row is recomputed before the one-click checks.

**Integration** (`tests/integration/tests/insights_*.rs`, added to `docs/TESTPLAN.md`):

- **Coverage** (`insights_coverage.rs`, profile `aliases`). The SimpleLogin seed writes a DKIM key for `news.example.com` and its TXT record in a CoreDNS zone. The test signs the invented newsletter itself, then delivers it through `sl-mx`, where OpenDKIM (verify mode, `AuthservID mx.example.org`) stamps `Authentication-Results`. With `mx.example.org` trusted, the signed newsletter is `aligned` with one-click available. An unsigned copy gives `not_aligned` (alignment is checked before coverage), a signature without `List-Unsubscribe-Post` in `h=` gives `not_covered`, and a copy with a prepended `List-Unsubscribe` gives `multiple_fields`.
- **One-click** (`insights_one_click.rs`, profile `docker`). The `images` nginx gains a TLS server on 443 with a certificate from the stack's `ca` service, and endpoints that accept the POST (200), answer a 302, and answer a 500, each request logged for inspection. `11.88.77.0/24` is public address space to `is_public`, so it cannot prove the allowed-network list; nginx instead joins two new internal networks with private subnets, `unsubscribe` (`10.88.79.0/24`, as `unsubscribe.it.test`) and `unlisted` (`10.88.80.0/24`, as `outside.it.test`). Holtmail runs in a container from a test image (`holtmail:it-insights`, built from the same Dockerfile with a new `FEATURES` build argument set to `insights-test-data`) on both. The test feature adds `[insights] one_click_test_networks` (CIDRs, here only the `unsubscribe` subnet) to `OneClick`; the release build has no such key. The suite asserts a multipart body and no cookie, auth, referrer or origin; a second request for the same list is `repeated` and sends nothing; a 302 is recorded as `redirected` and not followed; a 500 is `http_error` and a retry within the minute 429; the unlisted address is `refused`; `one_click = false` gives `disabled`; the logs carry no URL, host or list id.
- **Walk.** A walk SIGKILLed mid-archive resumes without re-parsing finished messages.
- **Data download** (`insights_data.rs`, profile `docker`, Phase 42). In the same container setup, the `images` nginx also answers as `data.it.test` and `mirror.it.test` on the `unsubscribe` network and serves signed releases from a per-run folder, with `/hop/` redirecting to `mirror.it.test` (pinned) and `/away/` to `outside.it.test` (not pinned). The base URL, pinned hosts, a 2 s interval and a 1 s delay are set by test config under `insights-test-data`, and the manifests are signed with the test key that feature compiles in. The suite asserts a failed check before anything is published; a release loaded through `GET /api/insights/data`, its files stored byte for byte and an archived message gaining a domain topic with no new import; the second fetch a `304` (`not_modified`); refusals for another key, an expired manifest and an older version that change nothing; after a restart, the stored release loaded at start and a `ut1` release through the redirect; an unpinned redirect refused with `download_refused` and nothing asked of the unpinned host; and no row domain, file hash, seed or subject in the logs. The first release loaded about 2 s after it was published.
  - `tests/integration/it.sh` builds `holtmail:it-insights` with `--build-arg FEATURES=insights-test-data` when the `docker` profile is on; `holtmail:dev` and the release image keep the empty default.
  - The server feature forwards to `category-domains/test-key`.
  - A CI check (`mise run release-features`: the Dockerfile default must be `ARG FEATURES=""`, and `cargo tree -e normal,build -p holtmail-server --locked -f '{p} {f}'` must show no `insights-test-data`, `test-networks` or `test-key`) fails if the feature is ever enabled there. The forward `-e features` tree would miss features the root's own features turn on.
- **Search.** `category:` search after an override (`tests/integration/tests/insights_search.rs`, Phase 40).
- **Access.** The `access.rs` matrix.

**VERIFY.md, "Insights":**

- `Authentication-Results` shapes and authserv-ids for Gmail, Fastmail, Outlook, iCloud and Yandex, and whether each strips forgeries of its own id. Presets ship only after this.
- One real one-click unsubscribe against a real sender (public DNS and CA, 2xx versus 3xx), and that the sender sees the server's address (§17.6).
- The consent dialog by eye in both themes and at narrow widths.
- Real airline, hotel and retail markup.
- Real pkpass files.
- The clipboard fallback over plain HTTP in Safari and Firefox.
- The card by eye in both themes and at narrow widths.
- Categories spot-checked on 100 messages of the maintainer's archive.
- The data download with signature: the production current and next keys, the first real signed release on `categories-latest`, and a real server fetching it from GitHub through the asset-host redirect with ETag and 304.
- Settings → About by eye in both themes and at narrow widths.

## 16. Coordination with Chats

| Shared piece | Owner | Rule |
|---|---|---|
| `evaluate_dependency` for html5ever (ammonia's version) | Chats Phase 0 / Insights Phase 35 | whoever lands first does it; the other ticks it |
| `holtmail-html-scan` | Insights | used by insights only; `holtmail-quote` keeps its html5ever DOM as CONVERSATIONS §6.1/§8.1 specify |
| `holtmail-links` | Insights (Phase 36) | Chats §5.4 `links` uses `unwrap` and `strip_tracking`; there is no second unwrapper |
| `holtmail-auth-results`, `Mail::dkim_pass_domains` | whoever lands first | `Mail` methods parse through `holtmail-auth-results` with unchanged semantics; no `holtmail-mime` dependency in `holtmail-alias` |
| `is_automated_local(&str) -> bool` | whoever lands first | the pinned exported API; `AUTOMATED` stays private |
| `holtmail_mime::body_parts` | Chats Phase 0 | Insights does not wait for it; adopting it bumps `holtmail_insights::VERSION` once, so a code in a second text part is found |
| Own addresses ("me") | Chats §4.1 | Insights stage B uses the same definition; until Chats lands, account addresses plus alias addresses |
| `walk.rs` | Insights Phase 35 | Chats §8.7 becomes the `chat-facts` step |
| Migrations | both | next free number at merge; reservations in docs are renumbered in the same change |
| WORKPLAN phases | both | Insights takes 35–42 now; Chats takes the next free numbers when its section is added |

**CONVERSATIONS edits**, made in the Phase 35 change:

- §4.5 (l.249-260): `is_automated_local(&str)` is the exported API (also l.255 and Phase 0 l.1098). Add a note that the insights kind differs from "automated" on purpose (§4.2 here).
- §5.4 (l.319): `links` takes distinct http(s) URLs from the clean text after `holtmail_links::unwrap` and `strip_tracking`.
- §8.1 (l.522) and Phase 0 (l.1096): `Mail::dkim_pass_domains` parses through `holtmail-auth-results`.
- §8.7/§8.8: the facts backfill becomes the `chat-facts` walk step (in the change that lands the walk).
- The migration numbers in §4.3 (l.215), §8.1 (l.527), the §8.2 heading (l.537), §8.4 (l.620) and §15 (l.1087, l.1110) become "next free at merge" if an insights migration lands first.

## 17. Open questions (with recommended answers)

1. **Name.** Is "Insights" right (`holtmail-insight`, `message_insights`, "Insights card")? *Recommended: yes.*
2. **Health topic.** On or off by default? *Recommended: on, local only, with `[insights.categories] health = false`, kept out of list chips, counts, webhooks, logs and exports.*
3. **Unverified mail and the domain file.** Should non-sensitive topics apply to unverified mail? *Recommended: yes. Many self-hosted providers stamp no trusted `Authentication-Results`. Finance, Health and Government stay gated, overrides included.*
4. **Relayed mail and sensitive topics.** Some alias services record the original sender's DMARC result in their own headers. *Recommended: not in this plan. Revisit with a per-service "verified original" signal behind an alias detector version.*
5. **Copying a code from the list row.** *Recommended: Later. A list button would fetch `messageQuery(id)` on click and copy from the detail DTO, so values never enter list payloads. Safari's clipboard after an async fetch needs `ClipboardItem` with a promise, which needs checking first.*
6. **One-click without a proxy.** It reveals the server's address to the sender. *Recommended: accept it. The consent dialog says "directly", and `[insights] one_click = false` turns the feature off.*
7. **UT1 at all.** It adds only shopping, jobs and social, and its upstream is down. *Recommended: build it but keep it opt-in through `sources`. Drop it if the mirror stops updating for 3 months.*
8. **Members.** *Recommended: members see kinds, topics, reservations, orders and parcels, but no codes, no sign-in or reset links and no unsubscribe links (§10).*
9. **Trust presets.** Ship provider presets for `trusted_authserv_ids`, or start empty? *Recommended: presets, each shipped only after its VERIFY entry confirms the provider strips forgeries of its own id.*
10. **Data file key name.** §8's `[insights.categories] sources = ["wikidata"]` clashes with the existing `sources` key, which names categorizers and rejects `wikidata` since Phase 39. *Resolved in Phase 42: the downloaded files' key is `data_sources`.*

## 18. Decisions

1. The base is the robust design. Two contracts plus plain libraries keep logic out of contract crates and the swap points where they belong.
2. The first card ships codes (Phase 35), because copying codes is what the maintainer asked for first.
3. Two category axes (kind plus topics) replace one category per message, so a travel newsletter answers both `is:newsletter` and `category:travel`.
4. Overrides are applied by the store into effective columns, per axis, never by read-time joins and never through a fingerprint. Edits touch only matching rows, and lists and counts are exact at once.
5. Trust, sender, coverage and categories are computed in stage B from stored auth facts and the stored alias row. Alias, PSL, trust-list and own-address changes never re-parse.
6. Reindex stays the global `REINDEX_SETTING` pass and fills missing insights rows itself. Category-driven reindex uses the durable `insight_reindex` queue (with `seq`) as a walk step woken at runtime. One schema bump, no per-message index version.
7. There is no `prior` chaining between extractors. Cross-extractor rules sit in the runner's merge, so each extractor's conformance suite covers its whole behaviour.
8. Work is bounded by input caps and candidate counts, never by `Instant`, so output stays deterministic. The golden snapshot catches dependency drift.
9. Trust has four states. Only `Authentication-Results` from operator-trusted or learned authserv-ids count, `l=` signatures count as unverified, and an alias service's signature vouches for delivery only. Mail with nothing to read is `unknown`, not `unverified`, so the card warns only when a check failed.
10. One-click uses its own direct connection with no proxy, pinned to checked public addresses on port 443, so `is_public` holds on the address actually contacted. Coverage needs both fields in `h=` and exactly one instance of each, not oversigning, which most senders do not do.
11. The data manifest is signed with ed25519 in a job separate from the build. It has key ids, expiry and a next key. Hashes from the same host prove consistency, not origin.
12. Only the CC0 file is bundled. BY-SA stays a separate opt-in download, and sensitive topics come only from reviewed Wikidata rows.
13. Freshness uses the earlier of `received_at` and `sent_at` on the client. Stale codes collapse with a reveal and are never hard-masked: the user opened the message.
14. Raw bytes are never rewritten. Forged-header defence comes from trust lists, not stripping.
