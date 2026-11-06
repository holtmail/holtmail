//! Contract for reading address books and calendars from an account's server: CardDAV and
//! CalDAV, Microsoft Graph and JMAP.
//!
//! Every protocol reduces to the same shape: an account has collections, a collection holds
//! items with a stable id and a version tag, and the server can say what changed since a token.
//! A source is read-only: it never changes anything on the server.

#[cfg(any(test, feature = "testing"))]
pub mod testing;

use async_trait::async_trait;
use bytes::Bytes;

/// The media types of [`RemoteItem::content_type`]: each source gives its items in the format its
/// server speaks, and the bytes are stored as they came.
pub mod media {
    use crate::CollectionKind;

    /// A vCard.
    pub const VCARD: &str = "text/vcard";
    /// An iCalendar object.
    pub const ICALENDAR: &str = "text/calendar";
    /// A Microsoft Graph `contact` as JSON.
    pub const GRAPH_CONTACT: &str = "application/vnd.microsoft.graph.contact+json";
    /// A Microsoft Graph `event` as JSON, a series master with its exceptions expanded.
    pub const GRAPH_EVENT: &str = "application/vnd.microsoft.graph.event+json";
    /// A JSContact card (RFC 9553), such as a JMAP `ContactCard`.
    pub const JSCONTACT: &str = "application/jscontact+json";
    /// A JSCalendar object (RFC 8984), such as a JMAP `CalendarEvent`.
    pub const JSCALENDAR: &str = "application/jscalendar+json";

    /// What kind of collection holds items of `content_type`, parameters and case ignored;
    /// `None` for a type that is none of the above.
    pub fn kind_of(content_type: &str) -> Option<CollectionKind> {
        let essence = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        match essence.as_str() {
            VCARD | GRAPH_CONTACT | JSCONTACT => Some(CollectionKind::Contacts),
            ICALENDAR | GRAPH_EVENT | JSCALENDAR => Some(CollectionKind::Calendar),
            _ => None,
        }
    }
}

/// What a collection holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CollectionKind {
    /// An address book of vCards.
    Contacts,
    /// A calendar of iCalendar objects: events, tasks or both.
    Calendar,
}

/// An address book or calendar on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCollection {
    /// Stable id of the collection on this server, such as its DAV path; the same collection has
    /// the same id in every listing.
    pub id: String,
    /// Address book or calendar.
    pub kind: CollectionKind,
    /// The name the server gives it, or a name made from its id when it gives none.
    pub name: String,
    /// The color the server gives it, as written, such as `#336699FF`.
    pub color: Option<String>,
    /// For calendars, the component types it may hold in upper case (`VEVENT`, `VTODO`); empty
    /// when the server does not say, and for address books.
    pub components: Vec<String>,
    /// A tag that changes whenever anything in the collection changes (a DAV `getctag` or
    /// `sync-token`); `None` when the server keeps none. Equal tags mean nothing changed.
    pub tag: Option<String>,
}

/// An item's id with its current version tag.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemTag {
    /// Stable id of the item in its collection, such as its DAV path.
    pub id: String,
    /// The version tag (an ETag): it changes whenever the item's bytes change. `None` when the
    /// server gives none.
    pub etag: Option<String>,
}

/// What changed in a collection since a token.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    /// Items added or changed since the token, each once.
    pub changed: Vec<ItemTag>,
    /// Ids of items removed since the token, each once. It may also name ids that were never in
    /// the collection (a server that tracks changes per account cannot tell where a destroyed
    /// item was); callers ignore those.
    pub removed: Vec<String>,
    /// The token to ask with next time.
    pub token: String,
}

/// One item's bytes exactly as the server sent them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteItem {
    /// Its id.
    pub id: String,
    /// Its version tag, when the server gave one with the bytes.
    pub etag: Option<String>,
    /// The media type, one of [`media`]: the same for every item of a collection.
    pub content_type: String,
    /// The item exactly as the server sent it: a vCard, an iCalendar object, or the JSON of one
    /// item.
    pub raw: Bytes,
}

/// How a source signs in. `Debug` never shows the secret.
#[derive(Clone, PartialEq, Eq)]
pub enum Credentials {
    /// A user name with a password or app password.
    Password {
        /// The user name.
        username: String,
        /// The password.
        password: String,
    },
    /// An OAuth 2.0 access token, sent as a bearer token. It expires, so a source is built with
    /// a fresh one before each sync.
    Bearer(String),
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Password { .. } => "Password(..)",
            Self::Bearer(_) => "Bearer(..)",
        })
    }
}

/// Why reading collections failed. Messages never hold names, contents or secrets.
#[derive(Debug, thiserror::Error)]
pub enum CollectionError {
    /// The server rejected the credentials. Retrying will not help until they change.
    #[error("sign-in rejected: {0}")]
    Auth(String),
    /// The server could not be reached, broke off or is busy. Retrying later may help.
    #[error("connection failed: {0}")]
    Connection(String),
    /// The server answered something unexpected.
    #[error("server error: {0}")]
    Protocol(String),
    /// The settings are incomplete or point at no address book or calendar.
    #[error("invalid settings: {0}")]
    Config(String),
    /// The server no longer accepts the token; [`CollectionSource::changes`] without a token
    /// or [`CollectionSource::list`] still work.
    #[error("the sync token expired")]
    TokenExpired,
    /// The collection cannot tell changes since a token; [`CollectionSource::list`] works.
    #[error("the server cannot list changes")]
    Unsupported,
    /// The collection is not on the server (any more).
    #[error("the collection is gone")]
    Gone,
}

impl CollectionError {
    /// Whether retrying the same call later can succeed.
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Connection(_) | Self::Protocol(_))
    }
}

/// What a [`CollectionWatch`] noticed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CollectionsChanged {
    /// Ids of the collections that may have changed, each once; `None` when the source cannot
    /// tell which, so every collection should be looked at.
    pub collections: Option<Vec<String>>,
}

/// A running watch on one account's collections. Not shared between tasks.
#[async_trait]
pub trait CollectionWatch: Send {
    /// Waits until something may have changed since the watch started or since the last call
    /// returned. A change made while the watch is alive is never swallowed; a return without a
    /// change is allowed. An error ends the watch; changes made before a new one starts are only
    /// found by a sync. Dropping the future while it waits gives the watch up.
    async fn next(&mut self) -> Result<CollectionsChanged, CollectionError>;

    /// Ends the watch. Dropping a watch without it is allowed.
    async fn close(self: Box<Self>);
}

/// A Web Push subscription (RFC 8030) that a server sends change notices to, encrypted for
/// these keys as RFC 8291 says. `Debug` never shows the auth secret.
#[derive(Clone, PartialEq, Eq)]
pub struct PushSubscription {
    /// The absolute https address notices are posted to.
    pub endpoint: String,
    /// The subscriber's P-256 public key, uncompressed (65 bytes).
    pub public_key: Vec<u8>,
    /// The 16-byte auth secret.
    pub auth_secret: Vec<u8>,
    /// When the subscriber would like the registration to end; the server may end it sooner.
    pub expires: Option<std::time::SystemTime>,
}

impl std::fmt::Debug for PushSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PushSubscription")
            .field("endpoint", &self.endpoint)
            .field("expires", &self.expires)
            .finish_non_exhaustive()
    }
}

/// A subscription a server accepted for one collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushRegistration {
    /// The registration's own address on the server, for [`CollectionSource::unsubscribe`].
    pub url: String,
    /// When the server stops sending to it, if it said.
    pub expires: Option<std::time::SystemTime>,
}

/// Reads one account's address books and calendars. Read-only, `Send + Sync`, usable as
/// `dyn CollectionSource`; calls may run one after another on the same value.
#[async_trait]
pub trait CollectionSource: Send + Sync {
    /// Every address book and calendar of the account, each once, in the server's order.
    /// Calendars that are only scheduling inboxes or outboxes are left out.
    async fn collections(&self) -> Result<Vec<RemoteCollection>, CollectionError>;

    /// What changed in `collection` since `token`. Without a token: every item now in the
    /// collection as changed, nothing removed, and a first token. The changes of one call are
    /// complete: asking again with the returned token lists only later changes. An item both
    /// changed and removed since the token is listed once, as removed. `TokenExpired` when the
    /// server no longer accepts `token`; `Unsupported` when the collection cannot list changes.
    async fn changes(
        &self,
        collection: &str,
        token: Option<&str>,
    ) -> Result<Changes, CollectionError>;

    /// The bytes of the items with these ids, in any order. Ids that are not (or no longer) in
    /// the collection are left out; each id is answered at most once.
    async fn fetch(
        &self,
        collection: &str,
        ids: &[String],
    ) -> Result<Vec<RemoteItem>, CollectionError>;

    /// Every item now in `collection` with its version tag, sorted by id, without the bytes:
    /// the recovery listing when changes since a token are not available.
    async fn list(&self, collection: &str) -> Result<Vec<ItemTag>, CollectionError>;

    /// Starts watching the account's collections on a connection of its own. `Ok(None)` (the
    /// default) when the server cannot push that way.
    async fn watch(&self) -> Result<Option<Box<dyn CollectionWatch>>, CollectionError> {
        Ok(None)
    }

    /// Asks the server to post a notice to `subscription` whenever `collection` changes.
    /// `Ok(None)` (the default) when the collection offers no such push, without asking. A new
    /// registration for the same endpoint replaces the old one. Notices are hints: the server
    /// may send one without a change, and none is sent once the registration expired.
    async fn subscribe(
        &self,
        collection: &str,
        subscription: &PushSubscription,
    ) -> Result<Option<PushRegistration>, CollectionError> {
        let _ = (collection, subscription);
        Ok(None)
    }

    /// Ends a registration from [`CollectionSource::subscribe`]. One the server no longer knows
    /// is not an error.
    async fn unsubscribe(&self, registration: &PushRegistration) -> Result<(), CollectionError> {
        let _ = registration;
        Ok(())
    }
}

#[cfg(test)]
mod watch_tests {
    use super::testing::{FakeCollections, watch_conformance};
    use super::*;

    #[tokio::test]
    async fn the_fake_keeps_the_watch_contract() {
        let fake = FakeCollections::new();
        fake.add_collection("/book/", CollectionKind::Contacts, "Book", None);
        assert!(fake.watch().await.unwrap().is_none(), "no push by default");
        fake.enable_watch();
        watch_conformance(&fake, &fake).await;
        assert_eq!(fake.watches(), 1);
        fake.break_watches(Some("cut"));
        assert!(fake.watch().await.is_err());
    }

    #[tokio::test]
    async fn subscriptions_replace_and_end() {
        let fake = FakeCollections::new();
        fake.add_collection("/book/", CollectionKind::Contacts, "Book", None);
        let sub = PushSubscription {
            endpoint: "https://holtmail.example.com/api/push/dav/a".into(),
            public_key: vec![4; 65],
            auth_secret: vec![1; 16],
            expires: None,
        };
        assert_eq!(fake.subscribe("/book/", &sub).await.unwrap(), None);
        fake.enable_push();
        let first = fake.subscribe("/book/", &sub).await.unwrap().unwrap();
        let second = fake.subscribe("/book/", &sub).await.unwrap().unwrap();
        assert_ne!(first.url, second.url);
        assert_eq!(
            fake.registrations().len(),
            1,
            "the same endpoint is replaced"
        );
        fake.unsubscribe(&second).await.unwrap();
        fake.unsubscribe(&second).await.unwrap();
        assert!(fake.registrations().is_empty());
        assert!(!format!("{sub:?}").contains("auth_secret"));
    }
}
