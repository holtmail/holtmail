# holtmail-mail-writeback

Sends held changes to messages on their servers ([WRITE.md, Mailbox changes](../../../docs/WRITE.md#mailbox-changes)). A change (flags, labels, a move, a delete, an append) is a row in the store's `mail_changes` from the moment it is asked for; `Writeback` takes the rows first in first out per account, at most one per message in flight, hands each to the account's `MailboxWriter`, and records whether the server accepted it, will be asked again after a temporary failure, or refused it. Flags and labels the server accepted are confirmed by what sync reads afterwards, and when the server shows something else the server wins. A library over `holtmail-store` (`MailChangeStore`), `holtmail-blob` and `holtmail-source` (`MailboxWriter`); the composition root supplies the `Writers` trait.

## Public API

### `Writers` (the caller implements it)

`writer(account, op) -> Result<Option<Arc<dyn MailboxWriter>>, SourceError>`: the account's writer when `op` may be sent now. `None` means writing this kind of change is not allowed (the account's capability is off, the instance switch closed) and the change fails with `not_allowed`; the server checks the capability per `MailOp::kind()`. An error counts like one from the writer (see the codes below), so a writer that cannot be built for a while (credentials being refreshed) answers with a transient error or `Auth`.

### `Writeback`

`Writeback::new(store, blobs, writers, WritebackConfig)`; `with_clock(f)` swaps the clock for tests.

`WritebackConfig` (defaults): `first_retry` 30 s doubling per attempt up to `max_retry` 1 h, `retry_for` 24 h after the change was asked for, `confirm_within` 15 min, `poll` 2 s, `settle_every` 60 s, `batch` 50 changes per account per pass.

| Method | Guarantees |
| --- | --- |
| `drain_account(account)` | Claims the account's due changes one at a time (`claim_mail_change`), oldest first, at most `batch`, and returns how many it attempted. A message's changes go out in the order they were made and never two at once; a change waiting for a retry or a sent flags change waiting for confirmation holds back the later changes of its message only. A rejected sign-in (`auth`) or a connection failure pauses the account: the pass stops and its other changes wait out the same backoff (`first_retry` doubling per failed pass up to `max_retry`, kept in memory, so a restart tries once at once), and an accepted or refused change ends the pause. A bad password costs one sign-in per backoff step, never one per held change |
| `drain_due()` | `drain_account` for every account with a due change that is not paused, accounts in parallel |
| `confirm(&[FlagObservation { folder, uid, flags, labels, read_at }]) -> Confirmed { applied, superseded }` | What sync read, passed to `confirm_mail_flags`: a change the state already satisfies is applied (held ones too); a change accepted before `read_at` that the state contradicts is superseded by the server; one being sent now or accepted after `read_at` is left alone, and so is every change newer than the first one that stays open. `read_at` must be taken before the read starts. Idempotent |
| `settle(account)` / `settle_all()` | Applies flags and labels changes accepted more than `confirm_within` ago that no sync confirmed, trusting the server's answer; returns how many |
| `recover()` | Puts changes left sending (not yet accepted) by a stopped process back to held and returns their ids. Call once at start, before `run`. An append among them may be saved twice |
| `run()` | `settle_all` every `settle_every`, `drain_due` in a loop, woken by `wake()` or every `poll`; runs until the process ends |
| `wake()` | Wakes `run` now; call it after enqueueing a change |

### What one attempt does

| Change | Writer call | Accepted when |
| --- | --- | --- |
| `Flags { add, remove }` | `set_flags(folder, [uid], add, remove, None)` with names parsed by `Flag::parse` | The UID is in `FlagResult::updated`; otherwise the message is gone (`gone`). Stays `sending` with `sent_at` until confirmed or settled |
| `Labels { add, remove }` | `set_labels([uid], add, remove)` | The call succeeds; waits for confirmation like flags |
| `Move { target }` | `move_to(folder, [uid], target)` | The call succeeds, also when the UID was no longer in the folder (a move sent again after a restart); `result_uid` is the UID in the target when the server reports it. Applied at once |
| `Delete { expunge }` | `delete(folder, [uid], expunge)` | The call succeeds. Applied at once |
| `Append { blob_id, flags, date }` | `append(folder, bytes, flags, date)` with the bytes from the blob store | The call succeeds; `result_uid` is the new UID when reported. Applied at once |

Folder ids are turned into paths through the store (`get_folder`) at each attempt.

**Following moves.** Changes are planned against the places the store knows, and sync only learns about a move later, so a change made right after a move (archive, then star) names the old folder and UID. Before each attempt the worker looks for an applied move of the same message from the change's place recorded before it (`applied_move_from`, followed up to 16 moves) and points the change at the move's target and reported UID (`retarget_mail_change`), so the server is asked at the right place and sync confirms flags there. A move to the folder the message already went to is applied without asking the server (`result_uid` is its UID there). When the earlier move reported no UID (IMAP without `UIDPLUS`), the change fails as `moved`.

### Outcomes and codes

`last_error` gets a short code, never server text, folder paths, addresses or subjects; logs carry change ids, the change kind, the code and attempt counts only.

| Code | Cause | Outcome |
| --- | --- | --- |
| `connection`, `protocol` | `SourceError::Connection`, `Protocol` | Held until the backoff ends; failed once the next attempt would fall after `retry_for` |
| `auth` | `SourceError::Auth` | Retried like the above: a rejected sign-in may be fixed by signing in again while the change waits. With `connection`, also pauses the whole account (see `drain_account`) |
| `store`, `blob` | The store or blob store failed while preparing the attempt | Retried |
| `config` | `SourceError::Config` | Failed |
| `unsupported` | `SourceError::Unsupported`, e.g. a keyword on Microsoft Graph | Failed |
| `missing_folder` | `SourceError::Missing`: a folder is not on the server | Failed |
| `unknown_folder` | The change's folder or a move's target is no longer in the store | Failed |
| `missing_bytes` | An append's blob is not in the blob store | Failed |
| `gone` | A flags change's UID is not in its folder any more | Failed |
| `moved` | An earlier move took the message to a UID its server did not report | Failed |
| `not_allowed` | `Writers::writer` answered `None` | Failed, without calling the server |
| `invalid` | A non-append change without a UID (the store refuses these on enqueue) | Failed |

`WritebackError::Store` is the only error the methods return; server and blob failures are recorded on the change.

### Policy and retention

The crate re-exports `MAIL_WRITE_CAPABILITIES`, `write_consent_missing` and `writes_mail` from [`holtmail-access`](../../access/docs/README.md), which holds their guarantees.

The rules the API and the server share, so they never disagree:

- `capability_for(op, to_trash, capabilities) -> Capability`: the lock a change needs. `mail.flags` for flags and labels, `mail.move` for a move (`mail.delete` for a move to the trash when the account lacks `mail.move`), `mail.delete` for a delete that only marks, `mail.expunge` for one that removes for good, `mail.append` for an append. `allows(op, to_trash, capabilities)` also wants `mail.delete` next to `mail.expunge`.
- `is_folder_label(name)`: whether a label name stands for a mailbox (`Inbox`, `Sent`, `Drafts`, `Trash`, `Spam` in any case, or any `\Name`). A labels change only needs `mail.flags`, so the API refuses these names and the IMAP writer answers `Unsupported` for them.
- `retention_days(account)`, `with_retention_days(settings, days)`: the account setting `delete_after_days` (`RETENTION_SETTING`), at most `MAX_RETENTION_DAYS` (36 500); other settings stay.
- `plan_retention(store, account, days, now, user, limit) -> RetentionPlan { changes, expunge, more }`: the changes that delete the account's messages older than `days` from its server, oldest first, at most `limit`, origin `retention`. With `mail.expunge`, deletes that remove for good (the trash too); otherwise moves to the account's trash folder (`trash_folder`), leaving what is already there. Only live located UIDs without an open change and without an earlier retention change count (`MailChangeStore::retention_candidates`), so planning again after recording is idempotent and a failed retention change is not recorded again every pass; it waits in the reader for _Try again_. Empty without `mail.delete`, or with neither `mail.expunge` nor a trash folder. Recording the plan is the caller's job.

## Limits

- No `unchanged_since`: flags are set unconditionally, and a concurrent change on the server is resolved by confirmation (the server wins), not by the write.
- A change accepted by the server and not yet marked in the store when the process stops is sent again after `recover`; flags, labels, moves and deletes are safe to repeat, an append is saved twice.
- A move or delete of a message that is no longer at its UID counts as applied; the next sync shows where the message is.

## Tests

`cargo test -p holtmail-mail-writeback` runs against `FakeSource::writer()` from `holtmail-source`'s `testing` feature and a SQLite store in a temporary directory: flags waiting for sync and the server winning, per-message order across a flags change and a move, backoff and giving up, refusals (unsupported keyword, gone UID, missing folder, no writer), appends from stored bytes and missing bytes, settling and recovery after a restart, the account pause after a failed sign-in (one attempt, then a doubling pause), changes following an applied move (retargeted, confirmed at the new place, a repeated move applied without a write), the lock each change needs, and retention plans (moves to the trash, expunges with a limit, no second plan for a place with an open change or a failed retention change). Against real servers through the binary in [`tests/integration/tests/mailbox_write.rs`](../../../tests/integration/tests/mailbox_write.rs).
