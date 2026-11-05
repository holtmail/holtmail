# holtmail-source

The contract for reading mail from an account's server. IMAP, Microsoft Graph and JMAP each implement it; the [sync engine](../../sync/docs/README.md) only talks to this contract.

## Read-only

A source never changes the server: no flags set, nothing moved, expunged or appended. For IMAP this means `EXAMINE` instead of `SELECT` and `BODY.PEEK[]` instead of `BODY[]`, so archiving does not mark mail as read.

## Traits

```rust
#[async_trait]
pub trait Source: Send + Sync {
    async fn connect(&self) -> Result<Box<dyn SourceSession>, SourceError>;
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

With the `testing` feature, `holtmail_source::testing::FakeSource` is a mail server in memory: add folders, mark one as a label folder, deliver messages (with labels, as Gmail's All Mail holds them, or with a received time, `deliver_received`), change their flags and labels as another client would, expunge them, renumber a folder (new `UIDVALIDITY`), make a folder's UIDs not ascend with arrival (`mark_unordered`) and deliver at a chosen UID (`deliver_at`), turn mod-sequences on (`enable_condstore`), keep a per-folder state that every delivery, flag change and expunge moves on (`track_changes`), count folder listings (`listings`), reject sign-in, and inspect which UIDs were fetched and which flag reads were made.

## Implementations

- [holtmail-source-imap](../../source-imap/docs/README.md)
- [holtmail-source-jmap](../../source-jmap/docs/README.md)
- [holtmail-source-graph](../../source-graph/docs/README.md)
