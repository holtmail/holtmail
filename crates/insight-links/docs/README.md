# holtmail-insight-links

The action-link extractor. `LinksExtractor` implements `holtmail_insight::InsightExtractor` with the id `links`: it scores the visible anchors of the HTML body and returns at most two `InsightKind::Action` items, the links the reader can act on (verify, confirm, activate, reset a password, sign in, magic link, view an order, track, check in, save to a wallet). The design is [INSIGHTS.md §7.3](../../../docs/INSIGHTS.md#73-action-links-insight-links).

## Public API

```rust
pub const ID: &str = "links";
pub const ALGORITHM: u32 = 1;

#[derive(Debug, Clone, Copy, Default)]
pub struct LinksExtractor;
impl LinksExtractor { pub fn new() -> Self }
impl InsightExtractor for LinksExtractor { /* id, version, extract, explain */ }
```

On top of the contract's guarantees (deterministic, pure, never panics, quiet on personal mail):

- **At most two items**, each an `InsightKind::Action` with provenance `Heuristic`, confidence `Likely`, origin `Html` and `forwarded: false`. The second item has a different `url` from the first.
- **Order.** Highest score first; equal scores by anchor index, then by position in the list.
- **Threshold.** An item needs at least the `likely` weight (600). Scores are clamped to `0..=1000`.
- **Action choice.** A Google Wallet save URL (`https://pay.google.com/gp/v/save/…`) is `wallet_save` whatever its text says; the JWT is never decoded. Otherwise the action of the text label wins, then the `aria-label`'s, then the URL token's. An anchor without an action is never returned, however high it would score.
- **Never an action:** `mailto:` links, URLs with an unsubscribe, preference or privacy token (also inside a wrapped URL), hrefs that do not parse as `holtmail_insight::Url`, URLs longer than `limits::MAX_DESTINATION_BYTES`, and anchors with `below_forward` (§7.0: heuristic actions come only from above the forward marker).
- **Fields.** `url` is the href as parsed (scheme `http(s)`, host as an A-label); `destination` equals `url`, `via` is `None` and `tracked` is `false`. `text_host` is the host of the anchor text as an A-label when the whole trimmed text reads as a host or an http(s) URL (`example.com`, `www.example.org/`, `https://accounts.example.com/login?next=1`, `example.org:8443/a`): an optional `http(s)://`, a host, an optional port, then optionally a path, query or fragment. No spaces, no userinfo, and not a number like `3.50` or `1.5/10` or an IP address. `label` is the trimmed anchor text cut with `truncate_display`, `None` when empty.
- **HTML only.** A message without an HTML part gives nothing in this phase; plain-text links are not read.
- **Version.** `version()` is `<ALGORITHM>+<first 12 hex chars of blake3(link-actions.toml)>`. `ALGORITHM` is bumped whenever the code changes behaviour.
- **Explain.** `explain(input)` lists, per anchor (as `at`, the anchor index), the rules that fired with their weights: `label` and `aria_label` (keyword: the matched label as written in the table), `button_like`, `path_token` (keyword: the token), `first`, `wallet`, `footer`, `social` (keyword: the table's domain), and the anchor's `score`; `mailto` and `unsubscribe` (keyword: the skip token) with score 0; `forwarded` for skipped anchors; then `candidates` (how many were kept) and one `chosen` step per item. Keywords come only from `link-actions.toml`; a keyword that happens to contain a returned label, URL or host is left out. Hrefs, hosts and anchor text never appear. `no_html` is the only step for a message without HTML.

## Scoring

Per anchor, with the weights of `[weights]`:

| Signal | Weight |
|---|---|
| The anchor text matches an action label | `label` +400 |
| The `aria-label` matches an action label | `aria_label` +300 |
| `button_like` (a background cell, padding or `role=button`, decided by `holtmail-html-scan`) | `button_like` +150 |
| A URL token in the href or its percent-decoded form | `path_token` +150 |
| The first anchor in the body that has an action | `first` +50 |
| A wallet save URL: the sum so far is raised to at least | `wallet` 600 |
| `in_footer` | `in_footer` −400 |
| A social host (the href's host or a host inside its decoded form, subdomains included) | `social` −300 |
| `mailto:`, or an unsubscribe, preference or privacy token | score 0, never an item |

The result is clamped to `0..=1000`. Only the first `limits::MAX_ANCHORS` (500) anchors are read; each costs at most one pass over an href of at most `MAX_URL_BYTES` and 512 chars of text and `aria-label`.

**Wrapped links.** Safelinks, Google `/url` and urldefense links carry the real target inside the href. The extractor reads the href twice: ASCII-lowercased as written, and percent-decoded (up to two rounds) and lowercased. URL tokens, skip tokens and social hosts are looked up in both, on every host, with no allow-list. This is only for scoring; the item's `url` is the href as written, and the authoritative unwrapping (`destination`, `via`, `tracked`) is done by the runner through `holtmail-links` on its exact host allow-list.

## Data file

`data/link-actions.toml` is embedded with `include_str!` and parsed once.

| Table | Contents |
|---|---|
| `[weights]` | `label` 400, `aria_label` 300, `button_like` 150, `path_token` 150, `first` 50, `in_footer` −400, `social` −300, `wallet` 600, `likely` 600 |
| `[labels.<action>]` | Labels per action (`verify`, `confirm`, `activate`, `reset_password`, `sign_in`, `magic_link`, `view_order`, `track`, `check_in`, `wallet_save`) for en, ru, de, fr, es, pt, it, zh, ja, ko, tr, pl; every action needs every language |
| `[paths]` | URL tokens per action (`/verify`, `/reset`, `/login`, `/track`, `/checkin`, ...); `any` tokens (`token=`, `otp=`) add the weight without naming an action |
| `[wallet]` | Wallet save URL prefixes |
| `[social]` | Social and messenger domains |
| `[skip]` | Unsubscribe, preference and privacy tokens |

Labels are matched NFKC-normalised and lowercased (whitespace collapsed, `’` read as `'`): whole words in spaced scripts, substrings when the label has CJK, kana, Hangul or Thai characters. When several labels match, the one that starts first wins, and among those the longest ("confirm your email" is `verify`, "confirm" alone is `confirm`). A label may name only one action; the parser refuses a table where it names two. A URL token that starts with `/` must start a path segment (not right after `/` or `:`, so `https://verify.example.com` is no match) and must not be followed by a letter or digit (`/verify-email` matches `/verify`, `/verifyx` does not); a token that ends with `=` must follow `?`, `&`, `;` or `#`. When several tokens match, a token with an action beats an `any` token, then the earliest one wins. If the embedded file ever failed to parse, `extract` returns nothing and `version()` ends in `+unreadable`; a unit test keeps that from shipping.

## What the runner and the API do instead

- `holtmail-insights` runs every action URL through `holtmail_links::unwrap` and sets `destination`, `via` and `tracked`, and drops a heuristic action that duplicates a schema.org one.
- The API computes `text_domain_mismatch`, `destination_differs_from_sender`, `confusable` and `display_host` at read time from `text_host` and `destination`.

## Limits

- Plain-text bodies are not read; a text-only reset mail gives no action.
- A wrapped wallet link is not recognised by its URL; it still becomes `wallet_save` through its label.
- A label in an image's `alt` is not text: an image-only button scores on `button_like`, `aria-label` and its URL alone (wallet links excepted).
- Anchors whose href exceeds `MAX_DESTINATION_BYTES` are dropped rather than shortened.

## Tests

```sh
cargo test -p holtmail-insight-links
```

- Unit tests: every weight, the first bonus, the threshold, clamping, footer, social (direct and wrapped), `mailto`, skip tokens (direct and wrapped), wallet links, the forward cut, the top-two rule and distinct URLs, ties by index, the action choice order, URL limits, item fields, `text_host` (including URL-shaped text with a path, port or query, IDN A-labels and non-hosts), a label per language, text-only input, the anchor cap, the matcher, URL tokens and decoding, and the data file.
- `tests/conformance.rs` runs `holtmail_insight::testing::conformance::extractor` with its own cases plus `personal_mail()` and `hostile_corpus()` (including 1,000 anchors), checks `id`/`version`, the two-item cap, and a proptest that extraction never panics and is deterministic on arbitrary anchor sets.
- `tests/fixtures.rs` reads the `.eml` fixtures in `tests/fixtures/` through `holtmail_mime` and `holtmail_insights::Runner::input`, and checks action, `url`, `text_host` and score: `link-reset-safelinks`, `link-urldefense-v3`, `link-lookalike-wrapper-host`, `link-esp-tracked`, `link-text-domain-mismatch`, `link-text-url-mismatch` (a full URL with a path as the visible text), `link-destination-differs`, `link-idn-homograph`, `link-wallet-save` give one action; `link-unsubscribe-not-action`, `link-forwarded-below-marker`, `link-footer-only` and `link-newsletter-shop-now` give nothing. `destination`, `via` and `tracked` for these fixtures are checked by the runner's tests. It also checks that `explain` never prints the fixture's hosts, URLs or label.
