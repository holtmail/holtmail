# holtmail-source

The contract for reading mail from an account's server. IMAP, Microsoft Graph and JMAP each implement it; the [sync engine](../../sync/docs/README.md) only talks to this contract.

## Read-only

A source never changes the server: no flags set, nothing moved, expunged or appended. For IMAP this means `EXAMINE` instead of `SELECT` and `BODY.PEEK[]` instead of `BODY[]`, so archiving does not mark mail as read.

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

## Credentials

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

`SourceError::is_transient()` says which ones are worth retrying.

## Testing

With the `testing` feature, `holtmail_source::testing::FakeSource` is a mail server in memory: add folders, mark one as a label folder, deliver messages (with labels, as Gmail's All Mail holds them, or with a received time, `deliver_received`), change their flags and labels as another client would, expunge them, renumber a folder (new `UIDVALIDITY`), make a folder's UIDs not ascend with arrival (`mark_unordered`) and deliver at a chosen UID (`deliver_at`), turn mod-sequences on (`enable_condstore`), keep a per-folder state that every delivery, flag change and expunge moves on (`track_changes`), count folder listings (`listings`), reject sign-in, and inspect which UIDs were fetched and which flag reads were made. `enable_watch` makes `watch` answer with a watch that reports the folders every delivery, flag change, expunge and renumbering touches; `break_watches` ends running watches with a `Connection` error (and fails new ones) until it is called with `None`; `reject_watch_sign_in` fails new watches with `Auth`; `watches` counts the watches started.

## Conformance for watches

A source that can push runs `holtmail_source::testing::watch_conformance(&source, &remote, folder, other).await` in its tests, where `remote` implements `testing::MailRemote` (`deliver(path)`, a new message in that folder, as an MTA would) against the same server and `folder` and `other` are two existing folders. It watches only `folder` and checks that a delivery wakes the watch and names the folder (or every folder), that each folder is named once, that later deliveries wake it again, and that a delivery to `folder` after one to `other` is not swallowed. `FakeSource` passes it.

## Implementations

- [holtmail-source-imap](../../source-imap/docs/README.md)
- [holtmail-source-jmap](../../source-jmap/docs/README.md)
- [holtmail-source-graph](../../source-graph/docs/README.md)
