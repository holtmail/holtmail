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
    async fn fetch(&mut self, uids: &[u64]) -> Result<Vec<RemoteMessage>, SourceError>;
    async fn logout(&mut self) -> Result<(), SourceError>;
}
```

| Item | Guarantees |
| --- | --- |
| `RemoteFolder { path, role, selectable }` | `path` uses `/` between levels whatever the server's delimiter; `selectable` is `false` for parents that cannot hold mail |
| `FolderStatus { uid_validity, uid_next, highest_modseq, exists }` | UIDs are comparable only while `uid_validity` stays the same |
| `uids()` | Every UID in the selected folder, ascending |
| `fetch(uids)` | Complete raw messages with flags and receive time, ascending by UID; UIDs that vanished are left out |

## Errors

| Error | Meaning | Retry |
| --- | --- | --- |
| `Auth` | Credentials rejected | No, until they change |
| `Connection` | Unreachable or connection broke | Yes, later |
| `Protocol` | Unexpected answer or refused command | Yes, later |
| `Config` | Account settings incomplete | No |

`SourceError::is_transient()` says which ones are worth retrying.

## Testing

With the `testing` feature, `holtmail_source::testing::FakeSource` is a mail server in memory: add folders, deliver and expunge messages, renumber a folder (new `UIDVALIDITY`), reject sign-in, and inspect which UIDs were fetched.

## Implementations

- [holtmail-source-imap](../../source-imap/docs/README.md)
