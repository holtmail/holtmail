# holtmail-insight-code

The one-time code extractor. `CodeExtractor` implements `holtmail_insight::InsightExtractor` with the id `code`: it reads a sign-in, verification or confirmation message and returns at most one `InsightKind::Code`. A WICG origin-bound last line (`@example.com #123456`) gives a certain code; otherwise a heuristic scores every code-shaped value and returns the best one only when it is unambiguous. The design is [INSIGHTS.md §7.1](../../../docs/INSIGHTS.md#71-codes-insight-code).

## Public API

```rust
pub const ID: &str = "code";

#[derive(Debug, Clone, Copy, Default)]
pub struct CodeExtractor;
impl CodeExtractor { pub fn new() -> Self }
impl InsightExtractor for CodeExtractor { /* id, version, extract, explain */ }
```

On top of the contract's guarantees (deterministic, pure, never panics, quiet on personal mail):

- **At most one item.** Always an `InsightKind::Code`, never `forwarded`.
- **Origin-bound codes.** When the last non-empty line of the text is `@host #code` (optionally followed by `@embedded-host`), sits above any forward marker, the host is an ASCII domain with a dot and the code is 1–64 ASCII letters, digits, `-` or `_`, the item is `Certain`, score 1000, provenance `OriginBound`, origin `Text`, with `origin_bound` set to the lowercase host. Nothing else is looked at.
- **Heuristic codes** have provenance `Heuristic`, a score below 1000, and are `Likely` at 600 or more (`[weights] likely`), `Possible` below. Their origin is the part of the best occurrence: `Html`, `Text` or `Subject`.
- **Version.** `version()` is `2+<first 12 hex chars of blake3(code-keywords.toml)>`, so any edit to the table changes it. The number before `+` is bumped whenever the code changes behaviour.
- **Explain.** `explain(input)` lists the steps of the same run as `holtmail_insight::Explanation`s: the gate (`gate:keyword` with the matched keyword, `gate:no_keyword`, `gate:list_id`, `gate:backup_list`, `gate:bulk_digits_only`, `gate:too_many_candidates`); every candidate occurrence with its part, char position, score, skeleton and nearest keyword, named `candidate` or after the hard-negative rule that zeroed it; each value by rank (`value`, with its positive occurrence count); and the outcome (`outcome:likely`, `outcome:possible`, `outcome:tie`, `outcome:list`, `outcome:all_negative`, ...), or `origin_bound`. Keywords are the table's own folded spelling; values, hosts and text never appear.

## How a heuristic code is found

1. **Source.** When the HTML view has visible segments, candidates come from the segments joined by line breaks; the text part then only confirms values (a bonus). Otherwise they come from the text, where each line is a unit "alone on its line". The subject is scanned as well. Text and subject are NFKC-normalised per char and lowercased for matching; fullwidth digits become ASCII and that is the stored value. `№` and `º` are kept as they are, so they stay reference marks rather than becoming `No`.
2. **Gate.** A keyword from the table in the subject or the first 4,096 chars of the body; no `List-Id` unless `Auto-Submitted` (other than `no`) is present; no backup-code phrase ("backup codes", "recovery codes", ...) in those places. The alphanumeric shape is used only when no bulk header is set: `Precedence: bulk|list|junk`, `Feedback-ID`, or an ESP header (`X-Mailgun-*`, `X-SES-*`, ... from the table) together with `List-Unsubscribe`.
3. **Candidates**, with boundaries checked in code (the chars around must not be ASCII letters or digits, so `验证码是123456` works):
   - a run of 4–8 ASCII digits;
   - `ddd ddd` or `ddd-ddd`, stored without the separator, unless a third group is joined with the same separator;
   - a run of 6–8 ASCII letters and digits with at least one of each, not a `#`-prefixed hex colour, not a word of any list in the table, not a barcode or format name (`Code128`, `EAN13`, ...).
4. **Hard negatives** score an occurrence 0. Each is a named rule (`Rule` in `src/extract.rs`) with a unit test and an input-level case:
   - `year_near_month`: a 4-digit 1900–2099 within 3 tokens of a month name (or `年`/`月`/`년`/`월`), or in a date shape like `2026-11-06`;
   - `money`: a currency symbol, ISO code or currency word within 3 tokens, or a decimal part (`1234.50`, `1.2345`);
   - `phone`: a `+` before the number, or a digit run joined by spaces, dashes, dots and brackets to 10 or more digits;
   - `postcode`: an address word within 3 tokens;
   - `reference_prefix`: a reference word or mark (`#`, `№`, `Nº`, `No.`, Order, Invoice, Заказ, Bestellung, ...) within 3 tokens before, unless a code keyword sits between it and the number ("Order verification code 482913" is a code);
   - `promo_keyword`: a promo word within 40 chars, as close as or closer than the nearest code keyword;
   - `in_url`: inside `http(s)://`, `www.`, `mailto:` or an `href=`, `src=` or `style=` value written in the text;
   - `footer`: at or after the footer marker (`HtmlView::footer_at`, or in text the first `©` or unsubscribe phrase);
   - `forwarded`: below the inline-forward marker (`forward_at`, `Segment::below_forward`).

   Token windows stop at line breaks. A token is a run of letters and digits in any non-CJK script, or a single CJK, kana, Hangul or Thai character, so "ご注文番号：" is four tokens before the number and "numaranız" is one.
5. **Score** (weights in `[weights]`): base 300; up to +300 for a keyword 0 chars away, falling linearly to 0 at 80; +150 when the value is also in the subject; +150 emphasis; +100 alone on its line; +50 within the first 1,024 chars of the body; +50 when the other body part has the value too; −100 for the alphanumeric shape; −150 for a 4-digit value from 1900 to 2099 (a year without a month nearby). Clamped to 0..=1000.
6. **Group** occurrences by value: the best occurrence's score plus 25 per extra positive occurrence, at most +75.
7. **Output** the best value only when it beats the best different value by 150 or more. Ties give nothing, and so does a list: three or more positive values of the same shape and length.

## Data file

`data/code-keywords.toml` is embedded with `include_str!` and parsed once. It holds:

| Table | Contents |
|---|---|
| `[weights]` | Every number above, plus the window sizes and caps |
| `[keywords]` | Code keywords for en, ru, de, fr, es, pt, it, zh, ja, ko, tr, pl (each language is required) |
| `[promo]` | Promo, coupon, discount, voucher, gift card and referral words per language |
| `[reference]` | Reference marks and per-language reference words |
| `[months]` | Month names and abbreviations per language |
| `[money]` | Currency symbols, ISO codes and currency words |
| `[address]` | Address words per language |
| `[footer]` | `©` and unsubscribe phrases (text footer marker) |
| `[backup]` | Backup and recovery code phrases |
| `[words]` | Alphanumeric words that are never codes |
| `[bulk]` | `Precedence` values and ESP header prefixes |

Phrases in spaced scripts match whole words; phrases with CJK, kana, Hangul or Thai characters match as substrings. A phrase written with a leading `-` also matches at the end of a longer word, for German compounds such as "Musterstraße" (`-straße`). When a hit of one list lies inside a strictly longer hit of another list, the longer one wins, so "promo code", "code postal" and "プロモーションコード" are not code keywords. If the embedded file ever failed to parse, `extract` returns nothing and `version()` ends in `+unreadable`; a unit test keeps that from shipping.

## Limits

- Text is read up to `limits::MAX_TEXT_BYTES` (256 KiB), segments up to `MAX_SEGMENTS` and the same 256 KiB of visible text, the subject up to 4 KiB. Reading all 2 MiB of segment text cost about 60 ms in release; 256 KiB keeps the extractor under 10 ms on hostile input. A value touching a cut is dropped, so no output is a prefix of a longer input value.
- More than 4,096 candidate occurrences in one part gives nothing.
- Token windows look at most 256 chars each way; "alone on its line" needs a line or segment of at most 256 chars.
- A code split across HTML segments (one digit per cell) is not joined: segments are separated by a line break.
- NFKC is applied per char, so a decomposed accent in the body does not compose; the runner already hands over NFKC text.
- Code-shaped numbers in personal mail that mentions a keyword ("the door code is 4512") can give a `Possible` code; that is what `Possible` is for.

## Tests

```sh
cargo test -p holtmail-insight-code
```

- Unit tests for the matcher, normalisation, URL marking, candidate shapes, every named hard-negative rule, the origin-bound line and the data file.
- `tests/conformance.rs` runs `holtmail_insight::testing::conformance::extractor` (plain personal mail and the hostile corpus included) with every §15 fixture of this crate recreated as an `InsightInput`, a sample per language, and an input-level case per hard negative, and checks the exact value found for each.
- `tests/fixtures.rs` reads the §15 `.eml` files in `tests/fixtures/` through `holtmail_mime::parse` and `holtmail_insights::Runner::input`, checks the code (or nothing) and its confidence for each, runs them through the conformance suite, and checks that `explain` names its rules and carries no value.
- The weights are tuned by the corpus gate in `holtmail-insights` (`cargo test -p holtmail-insights --test corpus`): when it fails, change the table or the rules here, not the labels. Any change to the table or the code changes the golden snapshot there as well.
