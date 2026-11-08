# holtmail-source

The contract for reading mail from an account's server. IMAP, Microsoft Graph and JMAP each implement it; the [sync engine](../../sync/docs/README.md) only talks to this contract.

## Read-only

A source never changes the server: no flags set, nothing moved, expunged or appended. For IMAP this means `EXAMINE` instead of `SELECT` and `BODY.PEEK[]` instead of `BODY[]`, so archiving does not mark mail as read. Changing the mailbox is the job of a separate [`MailboxWriter`](#mailboxwriter), which the server hands out only for accounts whose owner turned the matching capability on ([WRITE.md](../../../docs/WRITE.md#mailbox-changes)).

## Traits

```rust
#[async_trait]
pub trait Source: Send + Sync {
    async fn connect(&self) -> Result<Box<dyn SourceSession>, SourceError>;
    async fn watch(&self, folders: &[String]) -> Result<Option<Box<dyn SourceWatch>>, SourceError> { Ok(None) }
}

#[async_trait]
pub trait SourceWatch: Send {
    async fn next(&mut self) -> Result<Changed, SourceError>;
    async fn close(self: Box<Self>);
}

#[async_trait]
pub trait SourceSession: Send {
    async fn folders(&mut self) -> Result<Vec<RemoteFolder>, SourceError>;
    async fn select(&mut self, path: &str) -> Result<FolderStatus, SourceError>;
    async fn uids(&mut self) -> Result<Vec<u64>, SourceError>;
    fn state(&self) -> Option<String> { None }
    async fn unchanged_since(&mut self, state: &str) -> Result<Option<String>, SourceError> { Ok(None) }
    async fn fetch(&mut self, uids: &[u64]) -> Result<Vec<RemoteMessage>, SourceError>;
    async fn flags(
        &mut self,
        uids: RangeInclusive<u64>,
        changed_since: Option<u64>,
    ) -> Result<Vec<RemoteFlags>, SourceError>;
    async fn logout(&mut self) -> Result<(), SourceError>;
}
```

| Item | Guarantees |
| --- | --- |
| `RemoteFolder { path, role, selectable, label }` | `path` uses `/` between levels whatever the server's delimiter; `selectable` is `false` for parents that cannot hold mail; `label` is `true` for a label shown as a folder (Gmail), whose messages are all in the account's folder with the `All` role and carry the label there |
| `FolderStatus { uid_validity, uid_next, highest_modseq, exists, uids_ascend }` | UIDs are comparable only while `uid_validity` stays the same; `highest_modseq` is set when the server keeps mod-sequences (`CONDSTORE`), and only then does `flags` accept `changed_since`; `uids_ascend` says every new message gets a UID above all earlier ones (IMAP). Sources that derive UIDs from other ids (JMAP) set it `false`, and a sync then treats every UID it has not archived as new |
| `uids()` | Every UID in the selected folder, ascending |
| `state()` | An opaque token for the selected folder as the last `uids()` listed it (a Graph delta link), to store with the folder's cursor; never holds credentials. `None` by default, before a listing, and for sources without one (IMAP, JMAP) |
| `unchanged_since(state)` | `Some(token to keep)` when nothing in the selected folder was added, removed or changed since `state` was taken, flags included; `None` when something changed, the token is no longer accepted, or (the default) the source keeps no state. Never changes the selection |
| `fetch(uids)` | Complete raw messages with flags, labels, the server's own id and receive time, ascending by UID; UIDs that vanished are left out |
| `RemoteMessage { uid, flags, labels, server_id, received_at, raw }` | `labels` is `None` on servers without labels and the readable label names otherwise (`Inbox`, `Starred`, `Projects/2025`); `server_id` is an id kept across folders, Gmail's `X-GM-MSGID` |
| `flags(uids, changed_since)` | Flags and labels (no content) of the messages whose UIDs fall in the range, ascending; with `changed_since`, only those changed after that mod-sequence, and `Protocol` when the folder has no mod-sequences |
| `watch(folders)` | A watch on connections of its own for the named folder paths; a source that can only watch the whole account (JMAP) watches that. `Ok(None)` (the default) when the server cannot push; fails like `connect` when it cannot sign in |
| `SourceWatch::next()` | Returns once something in the watched folders may have changed since the watch started or the last return, saying where. A change made while the watch is alive is never swallowed; returns without a change are allowed. An error ends the watch; changes made before a new one starts are only found by a sync. Dropping the future while it waits gives the watch up |
| `Changed { folders }` | The folder paths that may have changed, each once; `None` when the source cannot tell which, so every folder should be looked at. `merge` joins two notices; `Changed::all()` is every folder |
| `SourceWatch::close()` | Ends the watch and signs out; dropping it without is allowed |

A watch is a hint to sync: it never archives anything and never says what changed in detail, so a missed or spurious notice can only make archiving later or a run useless.

## MailboxWriter

```rust
#[async_trait]
pub trait MailboxWriter: Send + Sync {
    fn capabilities(&self) -> MailboxWriteCaps;
    async fn set_flags(&self, folder: &str, uids: &[u64], add: &[Flag], remove: &[Flag],
                       unchanged_since: Option<u64>) -> Result<FlagResult, SourceError>;
    async fn set_labels(&self, uids: &[u64], add: &[String], remove: &[String]) -> Result<(), SourceError>;
    async fn move_to(&self, folder: &str, uids: &[u64], target: &str) -> Result<Moved, SourceError>;
    async fn append(&self, folder: &str, raw: &[u8], flags: &[Flag], date: Timestamp)
                    -> Result<Option<u64>, SourceError>;
    async fn delete(&self, folder: &str, uids: &[u64], expunge: bool) -> Result<(), SourceError>;
}
```

Every method works on its own connection or request and may run from several tasks at once. A call that fails with `Unsupported` or `Missing` changed nothing; any other failure may leave the call partly applied (some messages changed and others not, or a flags change with only its added flags set, or a copy-and-expunge move with the message in both folders). Every method except `append` is idempotent, so calling again with the same arguments finishes the job; JMAP, Graph and IMAP writers all rely on this. UIDs are those the account's `Source` reports for the folder.

| Item | Guarantees |
| --- | --- |
| `Flag` | `Seen`, `Answered`, `Flagged`, `Draft` or `Keyword(name)`; `as_imap()` spells it as IMAP does, `parse(name)` matches the four system flags ignoring ASCII case and keeps anything else (`\Deleted` included) as a keyword, `is(name)` compares ignoring ASCII case |
| `MailboxWriteCaps { flags, keywords, conditional, labels, move_uids, append_uid, mark_deleted }` | Fixed for the writer's lifetime. `flags` lists the system flags it can set (Graph: `Seen`, `Flagged`), `keywords` whether it takes keywords, `conditional` whether `set_flags` takes `unchanged_since`, `labels` whether `set_labels` works, `move_uids` and `append_uid` whether new UIDs are reported, `mark_deleted` whether `delete` can mark without expunging. Moving, expunging and appending always work. `full()` is everything; `supports(flag)` answers for one flag |
| `set_flags` | Idempotent: adding a flag a message has or removing one it lacks changes nothing and still lists the UID in `FlagResult.updated` (ascending). A flag in both lists ends up removed. UIDs not in the folder are ignored and listed nowhere. With `unchanged_since`, a message whose mod-sequence is above it is left alone and listed in `FlagResult.modified`. `Unsupported` before changing anything for a flag outside `supports` or `unchanged_since` without `conditional`; `Missing` for an unknown folder |
| `set_labels` | Labels by readable name on UIDs of the account's `All` folder; idempotent, remove wins, unknown UIDs ignored. `Unsupported` without `labels`, `Missing` without an `All` folder |
| `move_to` | Bytes and flags move along and the message leaves `folder`. `Moved.uids` pairs each moved UID with its new UID when reported, ascending; UIDs not in `folder` are left out, so moving again is harmless. `Missing` when either folder is unknown. Without a server-side move a failure between copy and expunge can leave the message in both folders, never in neither |
| `append` | Stores `raw` unchanged with the flags and `date` as received time. Not idempotent. Returns the new UID when the server tells it, always with `append_uid`. `Unsupported` for an unsupported flag, `Missing` for an unknown folder |
| `delete` | With `expunge` the messages are removed for good; without, marked `\Deleted` and still listed (needs `mark_deleted`, else `Unsupported`). Idempotent, unknown UIDs ignored, `Missing` for an unknown folder. Moving to Trash is `move_to` |


```rust
pub enum Credentials {
    Password(String),
    AccessToken(String),
}
```

How a source signs in: a password or app password, or an OAuth 2.0 access token (IMAP `AUTHENTICATE XOAUTH2`, a bearer token for JMAP and Microsoft Graph). Implementations take credentials in their constructor; access tokens expire, so the server builds the source with a fresh one before each sync. `Debug` prints only the variant, never the secret.

## Errors

| Error | Meaning | Retry |
| --- | --- | --- |
| `Auth` | Credentials rejected, including an access token the server refuses | No, until they change |
| `Connection` | Unreachable or connection broke | Yes, later |
| `Protocol` | Unexpected answer or refused command | Yes, later |
| `Config` | Account settings incomplete | No |
| `Unsupported` | The server or writer cannot do this (a keyword on Graph, `unchanged_since` without `CONDSTORE`); nothing changed | No |
| `Missing` | A folder the call names does not exist; nothing changed | No, until it exists |

`SourceError::is_transient()` says which ones are worth retrying.

## Testing

With the `testing` feature, `holtmail_source::testing::FakeSource` is a mail server in memory: add folders, mark one as a label folder, deliver messages (with labels, as Gmail's All Mail holds them, or with a received time, `deliver_received`), change their flags and labels as another client would, expunge them, renumber a folder (new `UIDVALIDITY`), make a folder's UIDs not ascend with arrival (`mark_unordered`) and deliver at a chosen UID (`deliver_at`), turn mod-sequences on (`enable_condstore`), keep a per-folder state that every delivery, flag change and expunge moves on (`track_changes`), count folder listings (`listings`), reject sign-in, and inspect which UIDs were fetched and which flag reads were made. `enable_watch` makes `watch` answer with a watch that reports the folders every delivery, flag change, expunge and renumbering touches; `break_watches` ends running watches with a `Connection` error (and fails new ones) until it is called with `None`; `reject_watch_sign_in` fails new watches with `Auth`; `watches` counts the watches started.

`FakeSource::writer()` is a `MailboxWriter` (`FakeWriter`) on the same server: its changes show in sessions, move mod-sequences and wake watches. `set_write_caps` makes it report and keep to other capabilities (`MailboxWriteCaps::full()` by default), `fail_writes` makes every call fail with `Connection` before changing anything, and `writes` counts the calls. `FakeSource` also implements `testing::MailboxProbe`.

## Conformance for writers

An implementation runs `holtmail_source::testing::writer_conformance(&writer, &probe, WriteFolders { folder, target, all }).await` against its in-process fake server. `probe` implements `testing::MailboxProbe`, which fills and reads the same server without the writer: `deliver(path, raw)` (an MTA's delivery, returns the UID) and `messages(path)` (every message as `Probed { uid, flags, labels, modseq, raw }`, ascending; `modseq` is required for a conditional writer). `folder` and `target` are two existing folders, which may hold other mail; `all` is the account's `All` folder and is needed when the writer has `labels`. The suite delivers messages with a `Message-ID` of their own on `example.com` and checks: `set_flags` with unknown UIDs, ascending `updated`, idempotency, removal, remove winning, no UIDs; keywords and system flags refused with `Unsupported` exactly when the capabilities say so, without changing anything; `unchanged_since` updating an unchanged message and reporting a changed one in `modified` (or `Unsupported` without `conditional`); labels added, kept once and removed (or `Unsupported`); a move keeping bytes and flags, leaving the folder, reporting the new UID with `move_uids` and being harmless twice; an append storing the exact bytes and flags and reporting the UID with `append_uid`; marking deleted (or `Unsupported`) and expunging, harmless twice; and `Missing` for unknown folders. `FakeWriter` passes it with full and with Graph-like capabilities.

## Conformance for watches

A source that can push runs `holtmail_source::testing::watch_conformance(&source, &remote, folder, other).await` in its tests, where `remote` implements `testing::MailRemote` (`deliver(path)`, a new message in that folder, as an MTA would) against the same server and `folder` and `other` are two existing folders. It watches only `folder` and checks that a delivery wakes the watch and names the folder (or every folder), that each folder is named once, that later deliveries wake it again, and that a delivery to `folder` after one to `other` is not swallowed. `FakeSource` passes it.

## Implementations

- [holtmail-source-imap](../../source-imap/docs/README.md)
- [holtmail-source-jmap](../../source-jmap/docs/README.md)
- [holtmail-source-graph](../../source-graph/docs/README.md)
