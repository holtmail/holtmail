# holtmail-ledger

The archive ledger: one entry for every message archived, every message deleted on purpose and every archive deleted, each sealed with a BLAKE3 hash over its fields and the hash of the entry before it. Changing, removing or reordering any entry changes every hash after it, so a head hash written down somewhere else proves that the history up to it is unchanged. A library, not a contract: the store keeps the entries ([`LedgerStore`](../../store/docs/README.md)), this crate seals and checks them.

## API

- `LedgerHash`: 32 bytes, written as 64 lowercase hex digits and read in either case; `ZERO` stands before the first entry.
- `LedgerEvent::{Archived, Deleted, ArchiveDeleted}` with `as_str`/`parse` (`archived`, `deleted`, `archive_deleted`).
- `NewEntry { at, event, archive_id, message_id, blob_id, actor }` and `NewEntry::seal(seq, prev) -> LedgerEntry`.
- `LedgerEntry { seq, at, event, archive_id, message_id, blob_id, actor, prev, hash }`; `expected_hash()` is what the fields give, equal to `hash` unless the entry was changed. Message events carry the message id and its blob id; `archive_deleted` neither. `actor` is the user who deleted, when a person did.
- `ChainCheck`: `new()`, `with_anchor(seq, hash)`, `push(&entry)` in stored order, `finish() -> ChainReport { entries, head, problems, problem_count }`. Only the last hash is kept, so a chain of any length is checked in constant memory. At most `MAX_PROBLEMS` (100) problems are listed; all are counted.
- `ChainProblem`: `Gap { expected, found }` (an entry missing or out of place), `BrokenLink { seq }` (the entry does not link to the hash before it), `Altered { seq }` (its fields do not give its hash), `AnchorMismatch { seq }`, `AnchorMissing { seq }` (the chain ends before the written-down position).

## The hash

BLAKE3 over `holtmail ledger v1\n`, then `seq` (u64, big-endian), `at` (Unix milliseconds, i64, big-endian), one event byte (1 archived, 2 deleted, 3 archive deleted), the archive UUID (16 bytes), the message UUID, the blob hash and the actor UUID (each a 0 byte when absent, else a 1 byte and the value), and the previous hash (32 bytes). Any change of encoding needs a new domain line.

## What it proves and what it does not

The chain shows changes to entries made after a head was written down elsewhere. Someone with write access to the database can rewrite the whole chain consistently, which only a head kept outside the instance (printed by `holtmail ledger head`, or sent by a webhook) can reveal. Entries hold ids and hashes, never mail content or addresses, so publishing a head reveals nothing.

## Tests

`cargo test -p holtmail-ledger`: an untouched chain, every field sealed, changed, resealed and missing entries named, anchors, the bounded problem list, hash text round trips.
