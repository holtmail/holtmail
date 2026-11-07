# holtmail-category-builder

A command-line tool that builds the category data files and signs their releases. `wikidata` builds `wikidata-categories.tsv.gz`, the CC0 file of registrable domains and their topics that `holtmail-category-domains` bundles: it asks the Wikidata Query Service which organisations belong to the industries and classes in `data/industries.toml`, keys each organisation's official website by its registrable domain, removes sensitive topics nobody has reviewed, checks the golden domains and writes the file in the `holtmail-category-data` format. `ut1` builds `ut1-categories.tsv.gz` (CC BY-SA 4.0) from a local checkout of the UT1 lists. `gate` runs the release sanity gate, `manifest` writes the unsigned release manifest, `sign` writes and signs it, `keygen` makes a signing key, and `check` checks an existing Wikidata file offline.

It is the only code in the workspace that reaches Wikidata or reads UT1. It runs by hand and in the weekly `categories` workflow; it never runs in the server. `publish = false`. It depends on `holtmail-category-data`, `holtmail-category` and `holtmail-links`, plus `ed25519-dalek`, `sha2` and `getrandom` for signing, never on an implementation crate.

## Commands

```sh
holtmail-category-builder wikidata --out <file.tsv.gz> [--previous <file.tsv.gz>] [--report <path>] \
    [--version <YYYYMMDD>] [--endpoint <url>] [--first-release]
holtmail-category-builder check <file.tsv.gz> [--previous <file.tsv.gz>]
holtmail-category-builder ut1 --dir <checkout> --out <ut1-categories.tsv.gz> [--previous <file.tsv.gz>] \
    [--report <path>] [--version <digits>] [--commit <sha>]
holtmail-category-builder gate --dir <release dir> [--previous <release dir>]
holtmail-category-builder manifest --dir <release dir> --key-id <id> --not-after-days <1..60> [--previous <release dir>]
holtmail-category-builder sign --dir <release dir> --key-id <id> --not-after-days <1..60> [--previous <release dir>]
holtmail-category-builder keygen --out <seed file> --key-id <id>
```

Every command exits non-zero on failure and prints one `error:` line; `gate`, `manifest` and `sign` also print one `gate:` line per broken rule.

`wikidata`:

- `--out` is replaced atomically: a temporary file in the same directory, then a rename. On any failure nothing is written, so a partial file never exists.
- `--previous` is read in full before anything is written, so it may be the same path as `--out`. Its header name must be `wikidata`. Its sensitive topics count as reviewed (see below).
- `--version` is digits only (1 to 16) and defaults to today's UTC date as `YYYYMMDD`. It must be numerically greater than the previous file's version, or the build fails. To build twice on one day, pass a larger number such as `2026110702`.
- `--report` receives the build report; without it the report goes to stdout. The report is written on success and on a gate failure.
- `--endpoint` defaults to `https://query.wikidata.org/sparql`. Only `https`, or plain `http` to a loopback address (tests), is accepted.
- `--first-release` is allowed only without `--previous`; see the sensitive gate.

The file has the header `#holtmail-categories 1 wikidata <version>`. The source column is `wikidata:Q<n>`, the smallest QID of the organisations that gave the domain, so equal input gives equal bytes. The build fails when the gzip is larger than 2 MiB (the bundle limit).

`check` reads a file and fails when its version is not greater than `--previous`'s, when a row carries a sensitive topic that neither the previous file nor `data/sensitive-reviewed.toml` gives that domain, when a golden domain is missing or has other topics, or when a row's domain, or a parent of it, is in golden `absent`, `skip_domains` or `mailbox_domains`. It prints the report to stdout and needs no network.

## UT1

`ut1` reads a checkout or an extracted tarball of [olbat/ut1-blacklists](https://github.com/olbat/ut1-blacklists), the mirror of the UT1 lists (the official site has been down since October 2026). `--dir` is the repository root (with `blacklists/<category>/domains`) or the `blacklists` directory itself. The builder never fetches it: the workflow clones it, and tests use hand-written lists in `tests/fixtures/ut1/` with invented hosts in the same layout.

- **Pinned input.** `data/ut1.toml` holds the repository URL, the pinned commit (`03501c64d230df9d84124176558d758351027be1`, upstream HEAD on 2026-11-07, committed upstream 2026-10-12) and the category mapping. Move the commit forward by hand after reading the upstream diff. `--commit` names the commit the checkout is at for a hand build; it defaults to the pinned one and is written into the report and `LICENSE-DATA`.
- **Mapping, non-sensitive only.** `shopping` → `purchases`, `jobsearch` → `jobs`, `social_networks` → `social`. Only mapped lists are read, so `bank`, `financial` and every other UT1 list never reach the output; `data/ut1.toml` refuses a sensitive topic. A mapped list that is missing or larger than 64 MiB fails the build, so an upstream rename never ships a silently smaller file.
- **Hosts.** Each non-empty line not starting with `#` is one host. A line with anything but letters, digits, `.`, `-` and `_` is skipped as "not a bare host name" (a URL, a port or user info never sneaks another host in). The rest go through the same site-key rule as Wikidata websites (`https://<host>/`): `www.` dropped, the registrable domain for a bare host, the full host for a subdomain, IDNA to A-labels, IP addresses and bare public suffixes dropped, and hosts at or under `skip_domains` or `mailbox_domains` dropped. Topics are unioned per key; rows are sorted; the source column is `ut1`.
- **Output.** The header is `#holtmail-categories 1 ut1 <version>`. The build fails, writing nothing, when a row sits on or under a golden `absent` domain, when the file would have more than 500,000 rows or 16 MiB, or, with `--previous` (a `ut1` file), when the version is not greater or the row count moved by more than 20%. It writes `LICENSE-DATA` next to `--out`: `data/LICENSE-DATA` with the repository and commit filled in.
- **A real build** (2026-11-07, pinned commit, a laptop, release build): 97,980 rows (purchases 36,909, jobs 60,362, social 709), 679,428 bytes gzip, under a second. 98,423 hosts read (jobsearch 60,740, shopping 36,968, social_networks 715); skipped 421 shared platforms, 15 mailbox providers, 6 without a registrable domain, 1 invalid key.

## Licensing

| File                         | License                                                                                         | Where                                  |
| ---------------------------- | ----------------------------------------------------------------------------------------------- | -------------------------------------- |
| `wikidata-categories.tsv.gz` | CC0 1.0                                                                                         | bundled in the server and published    |
| `ut1-categories.tsv.gz`      | CC BY-SA 4.0, attribution "Contains data from UT1 (Université Toulouse Capitole), CC BY-SA 4.0" | published only, next to `LICENSE-DATA` |

The UT1 file is never bundled into a binary and never committed. `data/LICENSE-DATA` is the notice template (attribution, the source, what was changed, the licence and its legal-code URL); the build fills in the commit.

## Release gate

`gate --dir` checks every data file in a release directory against the previous release in `--previous` (its `manifest.json` and data files; a missing or empty directory on the first release). It fails on each of these and reports all of them:

- no data file, or a `*.tsv.gz` that is not `wikidata-categories.tsv.gz` or `ut1-categories.tsv.gz`;
- a header name other than the one fixed for the file (`wikidata`, `ut1`), or files carrying different versions;
- a version not greater than the previous manifest's or any previous file's;
- a row count more than 20% away from the previous manifest's `rows` for that file (or the previous file's count when no manifest lists it); a file new in this release is not compared;
- Wikidata file: a golden domain missing or with other topics, a row on or under an `absent`, `skip_domains` or `mailbox_domains` domain, or more than 50 rows with a sensitive topic that neither the previous Wikidata file nor `sensitive-reviewed.toml` gives the domain;
- UT1 file: a row on or under those domains, or any row with a sensitive topic.

Other files in the directory (`LICENSE-DATA`, reports) are ignored.

## Signing

`sign` reads the seed from the environment variable `CATEGORIES_SIGNING_KEY`: exactly 64 hex characters (32 bytes), surrounding whitespace ignored. The value is never printed or logged and no error message contains it. It re-runs the gate on `--dir` against `--previous`, then writes into `--dir`:

- `manifest.json` from `holtmail_category_data::Manifest::to_json`: `format` 1, the files' common `version`, `--key-id`, `not_after` = now + `--not-after-days` (1 to 60) in whole seconds, and per file in fixed order its `name`, `sha256` of the compressed bytes, `size`, `license` (`CC0-1.0`, `CC-BY-SA-4.0`) and `rows` (data lines, header not counted);
- `manifest.json.sig`: the 64 raw bytes of the ed25519 signature over exactly those manifest bytes, checked with `verify_strict` before anything is written.

Nothing is written when the key, the gate or the manifest check fails. It prints the version, the key id, the public key and the signature, never the seed.

`manifest` takes the same arguments without a key: it re-runs the gate and writes only `manifest.json`, byte for byte what `sign` would sign with the same clock, and removes a `manifest.json.sig` left in `--dir`. The workflow uses it so that no Rust code runs where the key is.

`scripts/sign-manifest.sh <dir> <key id>` signs that manifest with stock tools only (bash, jq, perl, `sha256sum`, OpenSSL 3's `pkeyutl -rawin`), reading the seed from `CATEGORIES_SIGNING_KEY`. Before signing it refuses a manifest of more than 64 KiB, with unknown or missing fields, another `format` or `key_id`, a malformed `version`, a `not_after` in the past or more than 60 days ahead, or a file name, license, size or sha256 that does not match the files in `<dir>`. It hands the seed to OpenSSL as a PKCS#8 DER key on a pipe, never in a file or on a command line, checks the signature against the derived public key, writes `manifest.json.sig` and prints the version, key id and public key. Ed25519 is deterministic, so its signature equals the one `sign` writes for the same manifest and seed.

`keygen --out <path> --key-id <id>` writes a fresh random seed as 64 hex characters and a newline, mode 0600, and never replaces an existing file. It prints the key id, the public key as hex and as the `ReleaseKey { id, public: [..] }` item for `CURRENT_KEY` or `NEXT_KEY` in `crates/category-domains/src/manifest.rs`. Store the seed in the `CATEGORIES_SIGNING_KEY` secret, set the repository variable `CATEGORIES_KEY_ID` to the id, delete the file, and ship the public key in a release before the key signs anything (see "Keys and rotation" in the `holtmail-category-domains` README).

## Workflow

`.github/workflows/categories.yml` runs every Monday at 03:23 UTC and on `workflow_dispatch`, one run at a time. The workflow token has no permissions by default; each job asks for its own.

**`build`** (`contents: read`, no secrets):

1. Checks out the repository, sets up the toolchain with `mise` and the Rust cache, as CI does.
2. Picks the version `YYYYMMDDHH` (UTC), ten digits like the bundled `2026110701`, so it is always greater.
3. Downloads `manifest.json` and the data files of the `categories-latest` release with `gh release download` into `previous/`; on the first run there is none. When no previous Wikidata file exists, the bundled snapshot takes its place.
4. Runs `wikidata --previous`, clones `olbat/ut1-blacklists` at the pinned commit (`git fetch --depth 1 <url> <sha>`, the commit checked after checkout) and runs `ut1` (with `--previous` when the release has a UT1 file), then `gate`.
5. Uploads the artifact `categories` (both files and `LICENSE-DATA`) and, even on failure, `categories-reports`.

**`manifest`** (`needs: build`, `contents: read`, no secrets, never contacts Wikidata or UT1):

1. Checks out the repository and builds the builder from source with no Rust cache, so nothing the `build` job produced decides the manifest.
2. Downloads the `categories` artifact, and the previous release from `categories-latest` itself rather than trusting the build job's copy.
3. Runs `manifest --key-id "$CATEGORIES_KEY_ID" --not-after-days 60` (the gate again, then `manifest.json`) and uploads it as the artifact `categories-manifest`.

**`sign`** (`needs: manifest`, `contents: read`, the only job with a secret):

1. Checks out only `apps/category-builder/scripts` and downloads the two artifacts. It compiles nothing and runs no action outside `actions/*`; every action in the workflow is pinned to a commit.
2. Runs `scripts/sign-manifest.sh dist "$CATEGORIES_KEY_ID"`; `CATEGORIES_SIGNING_KEY` is in the environment of this step only.
3. Uploads `manifest.json.sig` as the artifact `categories-signature`.

**`publish`** (`needs: sign`, `contents: write`, no secrets):

1. Downloads the three artifacts; it checks nothing out and builds nothing.
2. Creates the release `categories-YYYYMMDD` at the run's commit with every file. `gh release create` uploads to a draft and publishes it once all assets are there. A second run on the same UTC day fails here because the dated tag exists.
3. Replaces the assets of the rolling `categories-latest` release in three sequential `gh release upload --clobber` calls: the data files and `LICENSE-DATA`, then `manifest.json.sig`, then `manifest.json`. On the first run it creates the release like the dated one. Neither release is marked as the repository's latest. Replacing an asset removes the old one first, so a server that checks during the few seconds of the upload can see a mismatch or a missing file; it refuses the release and tries again after its interval.
4. Downloads both releases again and compares every asset byte for byte with the signed files, failing the run on any difference.

## Data files

The first three are checked in under `data/`, compiled into the binary, and dedicated to the public domain (CC0 1.0), as each file's header says.

- **`industries.toml`**: `[[root]]` entries of `qid`, `label` (the English label as on Wikidata), `property` (`P452` or `P31`) and `topics`, plus `skip_domains`, shared platforms (site builders, free hosts, link shorteners), and `mailbox_domains`, consumer mailbox providers and ISPs people write from (`gmail.com`, `yahoo.com`, `mail.ru`, `qq.com`, `free.fr`, ...). Neither list's domains nor their subdomains are ever written: a topic on a mailbox domain would land on personal mail. Each list holds valid keys, each once.
- **`sensitive-reviewed.toml`**: `[[domain]]` entries of `name` and `topics`, sensitive topics only. It is the maintainer's review: only the maintainer adds entries, after checking each domain. It ships empty.
- **`golden.toml`**: `[[domain]]` entries of `name` and `topics`: stable organisation domains whose topics must not change, filled from the first real build, non-sensitive topics only; and `absent`, domains that must have no row, neither themselves nor any subdomain (mailbox providers).
- **`ut1.toml`** (CC0): the UT1 repository, the pinned commit and the category mapping (see UT1).
- **`LICENSE-DATA`**: the notice template published next to the UT1 file.

## Wikidata queries

1. **Root check.** One query fetches the English labels of every root (`VALUES ?item {…} ?item rdfs:label ?l FILTER(lang(?l) = "en")`). The build fails when a root has no English label or a label other than the one in `industries.toml`, so a mistyped QID never pulls in the wrong class.
2. **Subclasses.** One `SELECT ?c WHERE { ?c wdt:P279* wd:<root> }` per root. A class reached from several roots gets the union of their topics.
3. **Organisations, first pass.** `?org wdt:P452 ?class; wdt:P856 ?site`, in `VALUES` batches of 30 class QIDs, for the P452 roots.
4. **Organisations, second pass.** `?org wdt:P31 ?class; wdt:P856 ?site` for the P31 roots: airlines, hotels, hospitals, pharmacies, banks, agencies and other organisations without an industry.

Both passes leave out dissolved organisations (`FILTER NOT EXISTS { ?org wdt:P576 [] }`).

**HTTP.** GET with `format=json`, `Accept: application/sparql-results+json` and the User-Agent `holtmail-category-builder/<version> (https://github.com/holtmail/holtmail) reqwest/0.13`, as the Wikimedia User-Agent policy asks. One request at a time with a 0.5 s pause between requests and a 180 s timeout. On 429 and 503 the builder waits for `Retry-After` (seconds or an HTTP date, capped at 300 s), or backs off 2, 4, 8, 16 s without it; 500, 502, 504 and connection errors back off the same way. After 5 attempts the build fails. Any other status fails at once.

**Truncation check.** A response counts only if the whole body parses as one complete SPARQL JSON document (`head.vars` and `results.bindings`, nothing after it). The endpoint answers a query that runs out of time with status 200, a cut-off body and a Java stack trace; such a body, or a connection broken mid-body, fails the build at once and nothing is written.

## From websites to rows

For each organisation's official website (`build.rs`, pure):

- `http` and `https` only; the URL's path must be empty or `/` (a page below the root is usually a profile on a shared host);
- the host is lowercased and converted to an A-label (IDNA) and a leading `www.` is removed; the key is the registrable domain (`holtmail_links::registrable_domain`) when the host is that domain, and the full host otherwise, so `https://shop.example.org` gives a `shop.example.org` row and never tags `example.org` (the categorizer tries the exact host first);
- the key must pass `holtmail_category_data::is_valid_domain`, and neither it nor any parent domain may be in `skip_domains` (skipped as a shared platform) or `mailbox_domains` (skipped as a mailbox provider).

Topics are unioned per key and rows are sorted in byte order. The report counts skipped websites by reason and lists the domains with the most organisations, where a shared platform missing from `skip_domains` shows up.

## Sensitive gate

`finance`, `health` and `government` are sensitive (INSIGHTS §4.2, §8). A row keeps a sensitive topic only when:

- the previous release (`--previous`) had the same domain with that topic, or
- `data/sensitive-reviewed.toml` lists the domain with that topic.

Every other sensitive topic is removed from the row (the row is dropped if nothing is left) and listed under "unreviewed sensitive rows" in the report. When more than 50 rows are listed, the build fails and writes nothing, which forces a review.

**First release.** The first snapshot has no previous release, so every sensitive topic is unreviewed. `--first-release` writes the file with all of them stripped and does not fail on their count; the report lists them for the maintainer. The first snapshot therefore carries no sensitive topic until the maintainer reviews rows into `sensitive-reviewed.toml`.

## Golden domains

After the gate, every domain in `data/golden.toml` must be present with exactly its listed topics, and no row may sit on or under a domain in `absent` or `mailbox_domains`; otherwise the build fails and writes nothing. The report shows each miss with the expected and found topics ("no row" for an absent domain).

## Refreshing the snapshot

The bundled file is `crates/category-domains/data/wikidata-categories.tsv.gz`. Refresh it per release with:

```sh
mise run categories-snapshot
```

which runs

```sh
cargo run --release -p holtmail-category-builder -- wikidata \
  --previous crates/category-domains/data/wikidata-categories.tsv.gz \
  --out crates/category-domains/data/wikidata-categories.tsv.gz \
  --report target/categories-report.txt
```

Then read `target/categories-report.txt`, review the unreviewed sensitive rows (add the ones you accept to `data/sensitive-reviewed.toml` and run again), and review the diff of the decompressed file before committing:

```sh
git show HEAD:crates/category-domains/data/wikidata-categories.tsv.gz | gzip -dc > /tmp/old.tsv
gzip -dc crates/category-domains/data/wikidata-categories.tsv.gz > /tmp/new.tsv
diff /tmp/old.tsv /tmp/new.tsv | less
```

Always build the snapshot with this command so the compressed bytes come from the same `flate2` backend (see `holtmail-category-data`'s README); the content hash does not depend on it.

The **first release** was built once by hand, with no previous file:

```sh
cargo run --release -p holtmail-category-builder -- wikidata --first-release \
  --out crates/category-domains/data/wikidata-categories.tsv.gz \
  --report target/categories-report.txt
cargo run --release -p holtmail-category-builder -- check crates/category-domains/data/wikidata-categories.tsv.gz
```

## Tests

`cargo test -p holtmail-category-builder` runs offline, inside `mise run check`. Recorded SPARQL JSON fixtures in `tests/fixtures/` are hand-written with invented QIDs (`Q9…`) and reserved example domains; no real response is checked in. They cover subclass expansion, the label check and its failure, both passes and batching, URL normalisation and skip rules, union and sort, deterministic bytes, the sensitive gate (previous release, reviewed list, strip, more than 50, first release), golden pass and failure, the monotonic version and the size cap. UT1 tests run on `tests/fixtures/ut1/` (invented hosts, including `bank` and `financial` lists that must never be read): the mapping, skip and mailbox domains, IP and malformed lines, determinism under reordered lines and the nested `blacklists/` layout, a missing list, drift and version against `--previous`, `absent` domains and `LICENSE-DATA`. Release tests cover every gate rule, signing with the `test` seed from `crates/category-domains/tests/fixtures/test-signing-key.hex` and verifying with `ed25519-dalek` directly against the known public key, refusals that write nothing, key errors that never contain the value, and `keygen` (mode 0600, no overwrite). An in-process HTTP server on 127.0.0.1 checks that a truncated 200 body ending in a stack trace fails the build without writing a file, that a 429 with `Retry-After: 1` followed by a 200 succeeds, and that the client gives up after 5 attempts.

## Not here yet

- The production key pair: until the maintainer runs `keygen` and compiles the public keys into `holtmail-category-domains`, every real manifest is refused there.
- A first real signed release from the workflow.
- The weekly Wikidata build fails the 50-row sensitive gate until the maintainer reviews the stripped sensitive rows into `sensitive-reviewed.toml` (every one counts as new against a previous file that carries none).
