# holtmail-links

Pure link helpers for insights (action links, `docs/INSIGHTS.md` §6.1 and §7.3) and chats (§5.4 `links`): unwrapping of known redirect wrappers, removal of tracking parameters, detection of opaque ESP click trackers, registrable domains on the Public Suffix List, and the IDN display policy. There is no I/O, no network and no logging; every function is deterministic, takes any input and never panics. It is a plain library, not a contract.

## Public API

| Item | Guarantees |
|---|---|
| `pub const VERSION: &str = "1"` | Version of the code rules (unwrap decoders, matching). Bumped by hand on any behaviour change. |
| `pub fn version() -> &'static str` | `"<VERSION>+<12 hex>"`, the hex being the start of `blake3(tracking-params.toml ‖ 0 ‖ trackers.toml)`. Changes when the code version or either data file changes. **This is the value for `extract_fp`.** |
| `pub fn psl_version() -> &'static str` | First 12 hex digits of `blake3(public_suffix_list.dat)`. For `derive_fp`; it is not part of `version()`. |
| `pub const MAX_DEPTH: usize = 4` | Most wrappers `unwrap` removes. |
| `pub const MAX_INPUT_BYTES: usize = 8192` | Longest input, and longest decoded step, `unwrap` works on (same as the contract's `MAX_URL_BYTES`). |
| `pub struct Unwrapped { pub destination: String, pub via: Option<String> }` | Result of `unwrap`. |
| `pub fn unwrap(url: &str) -> Unwrapped` | Removes allow-listed wrappers in a loop, at most 4 deep; `via` names the outermost (`google.com`, `outlook.com`, `proofpoint.com`). Every decoded step must be an absolute `http(s)` URL with a host and no userinfo, else the whole result is the input unchanged with `via == None` (fail closed). Off the allow-list the result is always the input unchanged. An unwrapped destination is `url`-serialised (A-label host). |
| `pub fn strip_tracking(url: &str) -> String` | Removes listed parameters from an `http(s)` URL, keeping the others' order and bytes and the fragment. The input unchanged when nothing matches or it does not parse. |
| `pub fn tracker(url: &str) -> Option<&'static str>` | The provider name when host and path match `trackers.toml`; never by substring. |
| `pub fn registrable_domain(host: &str) -> Option<String>` | eTLD+1 as a lowercase A-label, ICANN and private sections, implicit `*` rule for unknown TLDs. Accepts Unicode or A-labels, any case, one trailing dot. `None` for IP literals, empty or single-label hosts, bare public suffixes, empty labels, hosts over 253 bytes and IDNA failures. The result is a label-aligned suffix of the input's A-label form and its own registrable domain. |
| `pub struct DisplayHost { pub host: String, pub confusable: bool }` | Result of `display_host`. |
| `pub fn display_host(host: &str) -> DisplayHost` | Unicode form when no label is confusable; A-label form with `confusable = true` otherwise. ASCII hosts and IP literals: lowercased, not confusable. IDNA failure or empty input: the input unchanged, `confusable = true`. |

## Unwrap allow-list

Hosts are compared after URL parsing (case-insensitive, A-label), exactly:

| Host | Path | Payload | `via` |
|---|---|---|---|
| `www.google.com` | `/url` | `q`, else `url` (form-decoded) | `google.com` |
| any `<labels>.safelinks.protection.outlook.com` | any | `url` (form-decoded) | `outlook.com` |
| `urldefense.proofpoint.com` | `/v1/url` | `u`, strictly percent-decoded | `proofpoint.com` |
| `urldefense.proofpoint.com` | `/v2/url` | `u` with `-`→`%`, `_`→`/`, then percent-decoded | `proofpoint.com` |
| `urldefense.com` | `/v3/__<url>__;<base64>!…` | `<url>` percent-decoded, each `*` replaced by the next character of the URL-safe base64 tail, `**X` by a run of 2–65 characters | `proofpoint.com` |

Not wrappers, by construction: `google.com/url`, `www.google.com.evil.example`, `safelinks.protection.outlook.com` itself or `….outlook.com.evil.example`, `urldefense.com@evil.example` (userinfo), v3 on `urldefense.proofpoint.com`, v1/v2 on `urldefense.com`, a trailing-dot host. A v3 payload with too few or unused markers, invalid base64 or a bad escape fails closed.

## Data files

| File | Content | Licence |
|---|---|---|
| `data/tracking-params.toml` | `prefixes` (`utm_`) and `exact` names (`gclid`, `fbclid`, `mc_eid`, `mc_cid`, `_hsenc`, `_hsmi`, `mkt_tok`, `yclid`, …), compared case-insensitively after form decoding | ours, CC0-1.0 |
| `data/trackers.toml` | `[[tracker]]` with `name`, `host` (exact, or `*.suffix` needing at least one more label) and optional `path` (prefix on a segment boundary) | ours, CC0-1.0 |
| `data/public_suffix_list.dat` | The Public Suffix List, downloaded from <https://publicsuffix.org/list/public_suffix_list.dat> on 2026-11-06 (`VERSION: 2026-11-06_09-01-47_UTC`, commit `94ec5df7`) | MPL-2.0 (Mozilla Foundation) |

The data files are embedded with `include_str!` and parsed once. A file that fails to parse yields an empty list rather than a panic; the crate's tests make sure the shipped files parse. To refresh the PSL, download it again with curl to the same path: `psl_version()` changes on its own. Any edit of the two TOML files changes `version()` on its own.

Trackers listed: SendGrid, Mailchimp, Mandrill, SparkPost, HubSpot, Customer.io, Salesforce Marketing Cloud (`click.exacttarget.com`, `*.exct.net`), Amazon SES, Postmark, Mailjet, Constant Contact, Campaign Monitor, Klaviyo. Mailgun and Braze track clicks on the customer's own domain (`email.<domain>/c/…`, `ablink.<domain>`) and cannot be recognised by host, so they are not listed; the same holds for Salesforce Marketing Cloud's branded `click.<domain>` hosts.

## Display policy

A label (after IDNA to Unicode) is confusable, following UTS 39 and Chromium's IDN policy as far as `unicode-security` allows, when it:

1. has a character outside the General Security Profile's allowed identifier set (`-` is allowed);
2. is neither single-script nor highly restrictive (Latin + Han + Hiragana/Katakana, Latin + Han + Bopomofo, Latin + Han + Hangul);
3. mixes Latin with another script and has a character of that script that is a potential mixed-script confusable;
4. has no ASCII letter and its UTS 39 skeleton is all ASCII (a whole-script confusable such as Cyrillic `аррӏе`).

Not done, compared with Chromium: per-TLD script allowances (for example Cyrillic look-alikes under `.ru`), the top-domain skeleton list, and Chromium's extra blocked characters.

## Limits

- `unwrap` does not enforce the contract's 2 KiB `destination` cap; the runner checks it with `Url::fits_destination` and keeps the original URL when it does not fit.
- Host matching for wrappers and trackers is on the parsed host; ports are ignored.
- Opaque trackers are only recognised, never followed (no network).

## Tests and fuzzing

- `cargo test -p holtmail-links`: unit tests next to each module (every wrapper, nesting, the depth cap, malformed payloads, look-alikes, tracking parameters, tracker hits and near misses, PSL ICANN/private/IDN/IP cases, display of legitimate and confusable IDNs), `tests/lookalikes.rs` and `tests/proptests.rs` (identity off the allow-list, idempotence on the destination, no panics, `registrable_domain` suffix and fixed point, `display_host` round trip).
- Fuzz target `unwrap` in `fuzz/` (its own workspace, pinned nightly in `fuzz/rust-toolchain.toml`). With a Homebrew `cargo` first on `PATH`, put the pinned toolchain first:

  ```sh
  cd crates/links/fuzz
  TC=$(dirname "$(rustup which --toolchain nightly-2026-10-01 rustc)")
  PATH="$TC:$PATH" RUSTC="$TC/rustc" cargo-fuzz run unwrap -- -max_total_time=60
  ```
