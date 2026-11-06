//! An in-memory [`CollectionSource`] for code that syncs collections, and the conformance suite
//! every implementation runs in its own tests.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use async_trait::async_trait;
use bytes::Bytes;

use crate::{
    Changes, CollectionError, CollectionKind, CollectionSource, ItemTag, RemoteCollection,
    RemoteItem,
};

/// Changes a server's collections behind a [`CollectionSource`], for [`conformance`].
#[async_trait]
pub trait Remote: Send + Sync {
    /// Creates the item `name` in the collection with id `collection`, or replaces its bytes.
    async fn put(&self, collection: &str, name: &str, raw: &[u8]);
    /// Removes the item `name`.
    async fn delete(&self, collection: &str, name: &str);
    /// Makes the server refuse every sync token handed out so far.
    async fn expire_tokens(&self);
    /// The bytes [`CollectionSource::fetch`] gives for the item `name` put with `raw`: `raw`
    /// itself, unless the server's format cannot carry it as it is (a JSON protocol wraps it in
    /// its item object).
    fn stored(&self, collection: &str, name: &str, raw: &[u8]) -> Vec<u8> {
        let _ = (collection, name);
        raw.to_vec()
    }
}

struct FakeItem {
    etag: u64,
    raw: Bytes,
}

struct FakeCollection {
    kind: CollectionKind,
    media: Option<String>,
    name: String,
    color: Option<String>,
    items: BTreeMap<String, FakeItem>,
    log: Vec<(u64, String)>,
}

#[derive(Default)]
struct State {
    collections: BTreeMap<String, FakeCollection>,
    seq: u64,
    generation: u64,
    no_sync: bool,
    reject: bool,
    fetches: Vec<Vec<String>>,
    listings: usize,
}

/// An in-memory server: collections, items with counting ETags, a change log per collection
/// for sync tokens, and switches for expired tokens, servers without changes and refused
/// sign-ins. Item ids are `<collection id><name>`.
#[derive(Default)]
pub struct FakeCollections {
    state: Mutex<State>,
}

impl FakeCollections {
    /// An empty server.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a collection with id `id`, or renames and recolors it.
    pub fn add_collection(&self, id: &str, kind: CollectionKind, name: &str, color: Option<&str>) {
        let mut state = self.state.lock().expect("fake lock");
        match state.collections.get_mut(id) {
            Some(existing) => {
                existing.name = name.into();
                existing.color = color.map(Into::into);
            }
            None => {
                state.collections.insert(
                    id.into(),
                    FakeCollection {
                        kind,
                        media: None,
                        name: name.into(),
                        color: color.map(Into::into),
                        items: BTreeMap::new(),
                        log: Vec::new(),
                    },
                );
            }
        }
    }

    /// Makes the items of collection `id` answer with `content_type` instead of `text/vcard` or
    /// `text/calendar`, as a server speaking another format does.
    pub fn set_media(&self, id: &str, content_type: &str) {
        if let Some(c) = self
            .state
            .lock()
            .expect("fake lock")
            .collections
            .get_mut(id)
        {
            c.media = Some(content_type.into());
        }
    }

    /// Removes a collection with its items.
    pub fn remove_collection(&self, id: &str) {
        self.state.lock().expect("fake lock").collections.remove(id);
    }

    /// Creates or changes the item `name` and returns its id.
    pub fn put_item(&self, collection: &str, name: &str, raw: &[u8]) -> String {
        let mut state = self.state.lock().expect("fake lock");
        state.seq += 1;
        let seq = state.seq;
        let coll = state
            .collections
            .get_mut(collection)
            .expect("a known collection");
        coll.items.insert(
            name.into(),
            FakeItem {
                etag: seq,
                raw: Bytes::copy_from_slice(raw),
            },
        );
        coll.log.push((seq, name.into()));
        format!("{collection}{name}")
    }

    /// Removes the item `name`.
    pub fn delete_item(&self, collection: &str, name: &str) {
        let mut state = self.state.lock().expect("fake lock");
        state.seq += 1;
        let seq = state.seq;
        let coll = state
            .collections
            .get_mut(collection)
            .expect("a known collection");
        if coll.items.remove(name).is_some() {
            coll.log.push((seq, name.into()));
        }
    }

    /// Makes every token handed out so far expired.
    pub fn expire(&self) {
        self.state.lock().expect("fake lock").generation += 1;
    }

    /// Makes [`CollectionSource::changes`] answer `Unsupported`, as a server without
    /// `sync-collection`.
    pub fn without_changes(&self) {
        self.state.lock().expect("fake lock").no_sync = true;
    }

    /// Refuses or accepts every later call's sign-in.
    pub fn reject_sign_in(&self, reject: bool) {
        self.state.lock().expect("fake lock").reject = reject;
    }

    /// The ids asked for by each [`CollectionSource::fetch`], in call order.
    pub fn fetches(&self) -> Vec<Vec<String>> {
        self.state.lock().expect("fake lock").fetches.clone()
    }

    /// How many [`CollectionSource::list`] calls were made.
    pub fn listings(&self) -> usize {
        self.state.lock().expect("fake lock").listings
    }

    fn check(state: &State) -> Result<(), CollectionError> {
        if state.reject {
            return Err(CollectionError::Auth("the fake refuses".into()));
        }
        Ok(())
    }

    fn tag(seq: u64) -> String {
        format!("\"{seq}\"")
    }
}

fn split(collection: &str, id: &str) -> Option<String> {
    id.strip_prefix(collection).map(str::to_owned)
}

fn content_type(kind: CollectionKind) -> &'static str {
    match kind {
        CollectionKind::Contacts => "text/vcard",
        CollectionKind::Calendar => "text/calendar",
    }
}

#[async_trait]
impl CollectionSource for FakeCollections {
    async fn collections(&self) -> Result<Vec<RemoteCollection>, CollectionError> {
        let state = self.state.lock().expect("fake lock");
        Self::check(&state)?;
        Ok(state
            .collections
            .iter()
            .map(|(id, c)| RemoteCollection {
                id: id.clone(),
                kind: c.kind,
                name: c.name.clone(),
                color: c.color.clone(),
                components: match c.kind {
                    CollectionKind::Calendar => vec!["VEVENT".into(), "VTODO".into()],
                    CollectionKind::Contacts => Vec::new(),
                },
                tag: Some(format!("ctag-{}", c.log.last().map_or(0, |(s, _)| *s))),
            })
            .collect())
    }

    async fn changes(
        &self,
        collection: &str,
        token: Option<&str>,
    ) -> Result<Changes, CollectionError> {
        let state = self.state.lock().expect("fake lock");
        Self::check(&state)?;
        if state.no_sync {
            return Err(CollectionError::Unsupported);
        }
        let coll = state
            .collections
            .get(collection)
            .ok_or(CollectionError::Gone)?;
        let next = format!("fake-{}-{}", state.generation, state.seq);
        let Some(token) = token else {
            return Ok(Changes {
                changed: coll
                    .items
                    .iter()
                    .map(|(name, item)| ItemTag {
                        id: format!("{collection}{name}"),
                        etag: Some(Self::tag(item.etag)),
                    })
                    .collect(),
                removed: Vec::new(),
                token: next,
            });
        };
        let since = token
            .strip_prefix("fake-")
            .and_then(|s| s.split_once('-'))
            .and_then(|(generation, seq)| {
                Some((generation.parse::<u64>().ok()?, seq.parse::<u64>().ok()?))
            })
            .filter(|(generation, _)| *generation == state.generation)
            .map(|(_, seq)| seq)
            .ok_or(CollectionError::TokenExpired)?;
        let touched: BTreeSet<&String> = coll
            .log
            .iter()
            .filter(|(seq, _)| *seq > since)
            .map(|(_, name)| name)
            .collect();
        let mut changes = Changes {
            token: next,
            ..Changes::default()
        };
        for name in touched {
            let id = format!("{collection}{name}");
            match coll.items.get(name) {
                Some(item) => changes.changed.push(ItemTag {
                    id,
                    etag: Some(Self::tag(item.etag)),
                }),
                None => changes.removed.push(id),
            }
        }
        Ok(changes)
    }

    async fn fetch(
        &self,
        collection: &str,
        ids: &[String],
    ) -> Result<Vec<RemoteItem>, CollectionError> {
        let mut state = self.state.lock().expect("fake lock");
        Self::check(&state)?;
        state.fetches.push(ids.to_vec());
        let coll = state
            .collections
            .get(collection)
            .ok_or(CollectionError::Gone)?;
        let mut seen = BTreeSet::new();
        Ok(ids
            .iter()
            .filter(|id| seen.insert(id.as_str()))
            .filter_map(|id| {
                let item = coll.items.get(&split(collection, id)?)?;
                Some(RemoteItem {
                    id: id.clone(),
                    etag: Some(Self::tag(item.etag)),
                    content_type: coll
                        .media
                        .clone()
                        .unwrap_or_else(|| content_type(coll.kind).into()),
                    raw: item.raw.clone(),
                })
            })
            .collect())
    }

    async fn list(&self, collection: &str) -> Result<Vec<ItemTag>, CollectionError> {
        let mut state = self.state.lock().expect("fake lock");
        Self::check(&state)?;
        state.listings += 1;
        let coll = state
            .collections
            .get(collection)
            .ok_or(CollectionError::Gone)?;
        Ok(coll
            .items
            .iter()
            .map(|(name, item)| ItemTag {
                id: format!("{collection}{name}"),
                etag: Some(Self::tag(item.etag)),
            })
            .collect())
    }
}

#[async_trait]
impl Remote for FakeCollections {
    async fn put(&self, collection: &str, name: &str, raw: &[u8]) {
        self.put_item(collection, name, raw);
    }

    async fn delete(&self, collection: &str, name: &str) {
        self.delete_item(collection, name);
    }

    async fn expire_tokens(&self) {
        self.expire();
    }
}

const ADA: &[u8] = b"BEGIN:VCARD\r\nVERSION:3.0\r\nUID:ada-1\r\nFN:Ada Example\r\nEMAIL:ada@example.com\r\nEND:VCARD\r\n";
const BEA: &[u8] = b"BEGIN:VCARD\r\nVERSION:3.0\r\nUID:bea-1\r\nFN:Bea Example\r\nTEL:+1 555 0100\r\nEND:VCARD\r\n";
const BEA_2: &[u8] = b"BEGIN:VCARD\r\nVERSION:3.0\r\nUID:bea-1\r\nFN:Bea Example\r\nTEL:+1 555 0177\r\nEND:VCARD\r\n";
const CLEO: &[u8] =
    b"BEGIN:VCARD\r\nVERSION:4.0\r\nUID:cleo-1\r\nFN:Cleo \xc3\x9cbel & <Co>\r\nEND:VCARD\r\n";
const DAN: &[u8] = b"BEGIN:VCARD\r\nVERSION:3.0\r\nUID:dan-1\r\nFN:Dan Example\r\nEND:VCARD\r\n";
const STANDUP: &[u8] = b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nBEGIN:VEVENT\r\nUID:standup-1\r\nDTSTAMP:20260101T090000Z\r\nDTSTART:20260105T090000Z\r\nSUMMARY:Standup\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

fn find(items: &[RemoteItem], raw: &[u8]) -> RemoteItem {
    items
        .iter()
        .find(|i| i.raw.as_ref() == raw)
        .cloned()
        .unwrap_or_else(|| panic!("no item with bytes {:?}", String::from_utf8_lossy(raw)))
}

fn ids(tags: &[ItemTag]) -> BTreeSet<String> {
    tags.iter().map(|t| t.id.clone()).collect()
}

/// Checks that `source` keeps the [`CollectionSource`] contract, changing its server through
/// `remote`. Panics on the first violation.
///
/// The server must start with exactly one address book and one calendar, both empty. The bytes
/// given to [`Remote::put`] are vCards and iCalendar objects; a server that speaks another format
/// stores them as the body it answers for that item, so that [`CollectionSource::fetch`] gives
/// them back unchanged.
pub async fn conformance(source: &dyn CollectionSource, remote: &dyn Remote) {
    let listed = source.collections().await.unwrap();
    assert_eq!(listed.len(), 2, "one address book and one calendar");
    let book = listed
        .iter()
        .find(|c| c.kind == CollectionKind::Contacts)
        .expect("an address book")
        .clone();
    let calendar = listed
        .iter()
        .find(|c| c.kind == CollectionKind::Calendar)
        .expect("a calendar")
        .clone();
    assert_ne!(book.id, calendar.id);
    assert!(!book.name.is_empty() && !calendar.name.is_empty());
    assert_eq!(
        source.collections().await.unwrap(),
        listed,
        "listings are stable"
    );

    remote.put(&book.id, "ada.vcf", ADA).await;
    remote.put(&book.id, "bea.vcf", BEA).await;
    remote.put(&book.id, "cleo.vcf", CLEO).await;
    remote.put(&calendar.id, "standup.ics", STANDUP).await;

    let listing = source.list(&book.id).await.unwrap();
    assert_eq!(listing.len(), 3);
    assert!(listing.windows(2).all(|w| w[0].id < w[1].id), "sorted");
    let all: Vec<String> = listing.iter().map(|t| t.id.clone()).collect();
    let fetched = source.fetch(&book.id, &all).await.unwrap();
    assert_eq!(fetched.len(), 3, "each id once");
    let (ada, bea, cleo) = (
        find(&fetched, &remote.stored(&book.id, "ada.vcf", ADA)),
        find(&fetched, &remote.stored(&book.id, "bea.vcf", BEA)),
        find(&fetched, &remote.stored(&book.id, "cleo.vcf", CLEO)),
    );
    for item in [&ada, &bea, &cleo] {
        assert_eq!(
            crate::media::kind_of(&item.content_type),
            Some(CollectionKind::Contacts),
            "a contact's media type"
        );
        assert_eq!(
            item.content_type, ada.content_type,
            "one type per collection"
        );
        let tag = listing.iter().find(|t| t.id == item.id).expect("listed");
        if item.etag.is_some() {
            assert_eq!(item.etag, tag.etag, "fetch and list agree on tags");
        }
    }
    let twice = source
        .fetch(&book.id, &[ada.id.clone(), ada.id.clone()])
        .await
        .unwrap();
    assert_eq!(twice.len(), 1, "an id asked twice is answered once");

    let event = source.list(&calendar.id).await.unwrap();
    assert_eq!(event.len(), 1);
    let event = source
        .fetch(&calendar.id, &[event[0].id.clone()])
        .await
        .unwrap();
    assert_eq!(event.len(), 1);
    assert_eq!(
        event[0].raw.as_ref(),
        remote.stored(&calendar.id, "standup.ics", STANDUP),
        "bytes as stored"
    );
    assert_eq!(
        crate::media::kind_of(&event[0].content_type),
        Some(CollectionKind::Calendar),
        "an event's media type"
    );

    let tag_before = source
        .collections()
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.id == book.id)
        .unwrap()
        .tag;

    let first = match source.changes(&book.id, None).await {
        Ok(first) => Some(first),
        Err(CollectionError::Unsupported) => None,
        Err(other) => panic!("changes without a token failed: {other}"),
    };
    if let Some(first) = &first {
        assert_eq!(ids(&first.changed), ids(&listing), "everything, once");
        assert!(first.removed.is_empty());
        assert!(!first.token.is_empty());
    }

    remote.put(&book.id, "bea.vcf", BEA_2).await;
    remote.delete(&book.id, "cleo.vcf").await;
    remote.put(&book.id, "dan.vcf", DAN).await;

    let after = source.list(&book.id).await.unwrap();
    assert_eq!(after.len(), 3);
    assert!(
        !after.iter().any(|t| t.id == cleo.id),
        "removed items leave"
    );
    let bea_tag = after.iter().find(|t| t.id == bea.id).expect("bea stays");
    if bea.etag.is_some() {
        assert_ne!(bea_tag.etag, bea.etag, "a change changes the tag");
    }
    if tag_before.is_some() {
        let tag_after = source
            .collections()
            .await
            .unwrap()
            .into_iter()
            .find(|c| c.id == book.id)
            .unwrap()
            .tag;
        assert_ne!(
            tag_after, tag_before,
            "a change changes the collection's tag"
        );
    }
    let gone = source
        .fetch(&book.id, &[ada.id.clone(), cleo.id.clone()])
        .await
        .unwrap();
    assert_eq!(gone.len(), 1, "removed ids are left out");
    assert_eq!(
        gone[0].raw.as_ref(),
        remote.stored(&book.id, "ada.vcf", ADA)
    );

    let Some(first) = first else {
        return;
    };
    let second = source.changes(&book.id, Some(&first.token)).await.unwrap();
    let dan = after
        .iter()
        .find(|t| t.id != ada.id && t.id != bea.id)
        .expect("dan is listed")
        .id
        .clone();
    assert_eq!(
        ids(&second.changed),
        BTreeSet::from([bea.id.clone(), dan.clone()]),
        "changed and added items"
    );
    assert_eq!(second.removed, vec![cleo.id.clone()]);
    let changed = source
        .fetch(&book.id, std::slice::from_ref(&bea.id))
        .await
        .unwrap();
    assert_eq!(
        changed[0].raw.as_ref(),
        remote.stored(&book.id, "bea.vcf", BEA_2)
    );

    let quiet = source.changes(&book.id, Some(&second.token)).await.unwrap();
    assert!(
        quiet.changed.is_empty() && quiet.removed.is_empty(),
        "nothing changed since the last token"
    );

    remote.expire_tokens().await;
    assert!(matches!(
        source.changes(&book.id, Some(&quiet.token)).await,
        Err(CollectionError::TokenExpired)
    ));
    let fresh = source.changes(&book.id, None).await.unwrap();
    assert_eq!(ids(&fresh.changed), ids(&after));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_fake_keeps_the_contract() {
        let fake = FakeCollections::new();
        fake.add_collection("/books/main/", CollectionKind::Contacts, "Contacts", None);
        fake.add_collection(
            "/cals/work/",
            CollectionKind::Calendar,
            "Work",
            Some("#336699"),
        );
        conformance(&fake, &fake).await;
    }

    #[tokio::test]
    async fn the_fake_without_changes_keeps_the_contract() {
        let fake = FakeCollections::new();
        fake.add_collection("/books/main/", CollectionKind::Contacts, "Contacts", None);
        fake.add_collection("/cals/work/", CollectionKind::Calendar, "Work", None);
        fake.without_changes();
        conformance(&fake, &fake).await;
    }
}
