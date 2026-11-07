# holtmail-category

The contract for message categorizers. A categorizer reads what stage A stored about a message (header signals, item kinds, reservation types, trust, sender) and votes on its **kind** (exactly one: `personal`, `list`, `bulk`, `notification`) and its **topics** (zero or more of a closed set). The runner combines the votes of every categorizer per axis by source rank and applies the trust gate. No body is read, nothing does I/O and nothing reads a clock. The design is [docs/INSIGHTS.md §4.2](../../../docs/INSIGHTS.md).

## API

```rust
pub use holtmail_insight::{KindTag, KindTags, Signal, Signals};

pub const TAXONOMY_VERSION: &str = "1";

pub trait Categorizer: Send + Sync {
    fn id(&self) -> &str;
    fn snapshot(&self) -> Arc<dyn CategorySnapshot>;
}
pub trait CategorySnapshot: Send + Sync {
    fn version(&self) -> String;
    fn categorize(&self, input: &CategoryInput) -> Option<CategoryVote>;
}

pub struct CategoryInput {
    pub signals: Signals, pub kinds: KindTags, pub reservations: ReservationKinds,
    pub structured: bool, pub forwarded: bool, pub own: bool, pub trust: TrustState,
    pub sender_address: Option<String>, pub sender_domain: Option<String>,
}
pub struct CategoryVote {
    pub kind: Option<MessageKind>, pub kind_source: CategorySource,
    pub topics: TopicSet, pub source: CategorySource,
}

pub enum MessageKind { Personal, List, Bulk, Notification }        // ALL, name, from_name
pub enum Topic { Purchases = 0, Travel = 1, Finance = 2, Health = 3,
                 Social = 4, Jobs = 5, Events = 6, Government = 7 }  // ALL, bit, from_bit, name, from_name, is_sensitive
pub struct TopicSet(u64);   // EMPTY, from_bits, bits, is_empty, contains, insert, remove, with, iter,
                            // known, list_topic, gated; FromIterator, From<Topic>, BitOr
pub enum TrustState { Aligned { domain: String }, Relayed { provider: String }, Unverified, Unknown }
                            // name, is_aligned
pub enum CategorySource { Override, Content, Domain, Headers }      // ALL, name, from_name, rank
pub enum ReservationKind { Flight, Lodging, Train, Bus, Event, Food, RentalCar }  // ALL, name, From<&Reservation>
pub struct ReservationKinds(u8);  // EMPTY, is_empty, contains, insert, remove, with, iter; FromIterator
pub enum CategoryError { Unavailable, Data(&'static str) }
```

Guarantees:

- **`Categorizer`** is object safe and `Send + Sync`. `id` is a stable, non-empty lowercase name. `snapshot` gives a consistent view of the current data: two snapshots with no reload in between have equal versions and give equal answers, and a snapshot taken before a reload keeps its old data and version.
- **`CategorySnapshot`**: its version and answers never change for its life. `version` is non-empty and includes the rule version and the hash of every loaded table. `categorize` is deterministic for equal input, does no I/O, reads no clock and never panics. `None` means the categorizer has nothing to say. A vote's topics hold known topics only and are **not** gated by trust; the runner gates them.
- **`CategoryInput`** holds no body: every field is a stored column or computed in stage B from stored rows, so categories can be reproduced without a blob read. `kinds` and `reservations` cover items at `Likely` or higher that are not forwarded; `structured` is set when a non-forwarded `Certain` item from JSON-LD, microdata or a pkpass is present.
- **`MessageKind`, `CategorySource`, `TrustState`** names are stored and never change. `CategorySource::rank` is the §4.2 precedence: override 3, content 2, domain 1, headers 0; a higher rank wins, per axis.
- **`Topic`** bit positions are persisted (`derived_topics`, `category_overrides.topics`), append-only and never reused. `is_sensitive` is true for finance, health and government.
- **`TopicSet`** is stored as an integer; unknown bits survive a round trip. `iter` yields known topics in bit order; `known` drops unknown bits. `list_topic` is the first member in bit order that is not health (§5).
- **`TopicSet::gated(trust, health)`** drops sensitive topics unless `trust` is aligned, health when `health` is false, and unknown bits (this build cannot tell whether they are sensitive). It is the one gate for every source, overrides included.
- **`TAXONOMY_VERSION`** changes whenever a kind or topic is added or its meaning changes; it joins `derive_fp`.
- **`CategoryError`**'s `Display` names only the failed rule, never content, senders or table rows.

### Additions to INSIGHTS.md §4.2

- **`CategoryInput::reservations`** (`ReservationKinds`, not persisted). The stored `kinds` bits say only `reservation`, which cannot tell an event ticket (topic `events`) from a trip (topic `travel`). Stage B rebuilds the reservation types from the stored items with the same filter as `kinds`.
- **`CategoryVote::kind_source`**. `category-signals` speaks for two layers, content (topics and the content `notification`) and headers (the header kind), so the kind carries its own source next to `source`, the source of the topics.
- **`TopicSet::gated`** is logic in a contract crate. The store implementation must apply the same gate to overrides (§4.5) and may depend only on contracts, so this one pure, documented method lives here rather than in the runner.

## Implementing a categorizer

An implementation depends on `holtmail-category` (and `holtmail-core` if it needs it), never on another implementation. It must:

- keep every guarantee above, whatever the input holds: unknown signal and kind bits, 10 KiB, non-ASCII or empty senders, any trust state;
- put every table it reads into `version`, and reload only by building a new snapshot;
- leave the trust gate and precedence to the runner.

Run the conformance suite from the implementation's tests, with `holtmail-category = { workspace = true, features = ["testing"] }` under `[dev-dependencies]`:

```rust
holtmail_category::testing::conformance::categorizer(&MyCategorizer::new(), &cases);
```

`cases` is `&[(CategoryInput, Option<CategoryVote>)]`. The suite checks a stable lowercase id, equal versions across two snapshots, every expected vote (equal on two runs and on both snapshots), and that a deterministic hostile set never panics, never changes between runs and never gives unknown topic bits: every combination of the 13 known signal bits with the other fields varied, unknown high bits, long, non-ASCII and empty senders, and every trust state.

`testing::FakeCategorizer::new(id, version, votes)` answers `votes[sender_domain]`; `swap(version, votes)` replaces both like a reload, and snapshots taken earlier keep theirs. Runner and server tests use it for a mid-pass table change.

```sh
cargo test -p holtmail-category
```
