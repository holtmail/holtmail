# holtmail-category-signals

A `Categorizer` (id `signals`) that reads only the stored header signals and insight kinds of a message. It gives the kind from the header rules, `notification` from content when the content gate allows it, and the topics the insight kinds imply. It speaks for two layers of [docs/INSIGHTS.md §4.2](../../../docs/INSIGHTS.md): content and headers. It depends only on `holtmail-category`.

## API

```rust
pub const ID: &str = "signals";
pub const VERSION: &str = "1";

#[derive(Debug, Clone, Default)]
pub struct SignalsCategorizer { .. }        // SignalsCategorizer::new(); impl Categorizer

pub fn header_kind(input: &CategoryInput) -> MessageKind;
pub fn content_topics(input: &CategoryInput) -> TopicSet;
```

On top of the `Categorizer` contract, `categorize` always votes: `kind` is always `Some`, `source` (of the topics) is always `content`, and `kind_source` is `content` for a content `notification` and `headers` otherwise. The data is compiled in, so every snapshot is equal and its version is `VERSION`.

## Rules

**Header kind** (`header_kind`), first match wins:

1. `personal` when `own`, or the `auto_replied` bit (an out-of-office is a person's mail).
2. `list` when `list_id` and `list_post` (a `List-Post` other than `NO`; `list_post_no` is the `NO` form).
3. `bulk` when `unsubscribe` together with `precedence_bulk`, `feedback_id`, `esp`, or `list_id` without `list_post`.
4. `notification` when `auto_submitted`, `noreply`, or `esp` without `unsubscribe`.
5. `personal` otherwise. So `Precedence` alone or `List-Unsubscribe` alone stays `personal`.

**Content kind.** `notification` with `kind_source` content when all hold:

- not `own` and not `forwarded`;
- there is content: `structured`, a reservation type, or an item kind other than `unsubscribe` (every newsletter carries an unsubscribe item, so it is no evidence);
- the header kind is `bulk` or `notification`, or `structured` is set.

Mail a person wrote stays `personal`, and so does a forward of a receipt and the user's own mail. A receipt sent through an ESP becomes `notification`.

**Content topics** (`content_topics`), from `kinds` and `reservations`:

| Item | Topic |
|---|---|
| flight, lodging, train, bus or rental-car reservation | `travel` |
| event reservation | `events` |
| food reservation | none |
| order or parcel | `purchases` |
| invoice | `finance` |

They apply on forwarded messages too, because `kinds` and `reservations` already leave forwarded items out; only the content kind is withheld on a forward. The vote never applies the trust gate: a spoofed invoice still votes `finance`, and the runner drops it unless trust is aligned.

## Version policy

`VERSION` is bumped whenever any rule above changes, so `derive_fp` changes and stage B recomputes. A change to which signal bits stage A computes is a stage-A change and belongs to the runner's version.

## Tests

- Unit tests per rule in `src/tests.rs`.
- `tests/conformance.rs` runs `holtmail_category::testing::conformance::categorizer`.
- `tests/fixtures.rs` reads the invented `.eml` files in `tests/fixtures/`, runs stage A with `holtmail_insights::Runner` over the real extractors, builds a `CategoryInput` with a trust given per fixture, and checks the vote. The runner's tests read the same files to check trust and the gate.

```sh
cargo test -p holtmail-category-signals
```
