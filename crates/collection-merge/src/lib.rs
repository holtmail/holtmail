//! Two [`CollectionSource`]s presented as one: the address books of one and the calendars of
//! the other, such as a JMAP account's contacts with its calendars over CalDAV when the JMAP
//! session offers no calendars. Collection ids pass through unchanged, so collections stored
//! from either source keep their ids.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use holtmail_collection::{
    Changes, CollectionError, CollectionKind, CollectionSource, CollectionWatch,
    CollectionsChanged, ItemTag, PushRegistration, PushSubscription, RemoteCollection, RemoteItem,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Contacts,
    Calendars,
}

/// Address books from `contacts`, calendars from `calendars`.
///
/// Each call on a collection goes to the source that listed it; an id neither listed is
/// looked for in a fresh listing, then answered with `Gone`. When both sources list the same id,
/// the address book wins and the calendar is left out. Listing fails when either source fails.
pub struct MergedCollections {
    contacts: Box<dyn CollectionSource>,
    calendars: Box<dyn CollectionSource>,
    owners: Mutex<HashMap<String, Side>>,
    registrations: Mutex<HashMap<String, Side>>,
}

impl MergedCollections {
    /// The address books of `contacts` and the calendars of `calendars`; whatever else either
    /// lists is left out.
    pub fn new(contacts: Box<dyn CollectionSource>, calendars: Box<dyn CollectionSource>) -> Self {
        Self {
            contacts,
            calendars,
            owners: Mutex::new(HashMap::new()),
            registrations: Mutex::new(HashMap::new()),
        }
    }

    fn source(&self, side: Side) -> &dyn CollectionSource {
        match side {
            Side::Contacts => &*self.contacts,
            Side::Calendars => &*self.calendars,
        }
    }

    async fn owner(&self, collection: &str) -> Result<&dyn CollectionSource, CollectionError> {
        let known = self
            .owners
            .lock()
            .expect("owners lock")
            .get(collection)
            .copied();
        let side = match known {
            Some(side) => side,
            None => {
                self.collections().await?;
                self.owners
                    .lock()
                    .expect("owners lock")
                    .get(collection)
                    .copied()
                    .ok_or(CollectionError::Gone)?
            }
        };
        Ok(self.source(side))
    }
}

#[async_trait]
impl CollectionSource for MergedCollections {
    async fn collections(&self) -> Result<Vec<RemoteCollection>, CollectionError> {
        let books = self.contacts.collections().await?;
        let calendars = self.calendars.collections().await?;
        let mut owners = HashMap::new();
        let mut out = Vec::new();
        for (side, kind, listed) in [
            (Side::Contacts, CollectionKind::Contacts, books),
            (Side::Calendars, CollectionKind::Calendar, calendars),
        ] {
            for collection in listed.into_iter().filter(|c| c.kind == kind) {
                if owners.contains_key(&collection.id) {
                    tracing::warn!("a calendar shares its id with an address book and is left out");
                    continue;
                }
                owners.insert(collection.id.clone(), side);
                out.push(collection);
            }
        }
        *self.owners.lock().expect("owners lock") = owners;
        Ok(out)
    }

    async fn changes(
        &self,
        collection: &str,
        token: Option<&str>,
    ) -> Result<Changes, CollectionError> {
        self.owner(collection)
            .await?
            .changes(collection, token)
            .await
    }

    async fn fetch(
        &self,
        collection: &str,
        ids: &[String],
    ) -> Result<Vec<RemoteItem>, CollectionError> {
        self.owner(collection).await?.fetch(collection, ids).await
    }

    async fn list(&self, collection: &str) -> Result<Vec<ItemTag>, CollectionError> {
        self.owner(collection).await?.list(collection).await
    }

    /// Both sources' watches as one; `None` when neither watches. A notice from one names its
    /// collections or, when it cannot tell, every collection; an error from either ends it.
    async fn watch(&self) -> Result<Option<Box<dyn CollectionWatch>>, CollectionError> {
        let watches: Vec<Box<dyn CollectionWatch>> =
            [self.contacts.watch().await?, self.calendars.watch().await?]
                .into_iter()
                .flatten()
                .collect();
        match watches.len() {
            0 => Ok(None),
            1 => Ok(watches.into_iter().next()),
            _ => Ok(Some(Box::new(MergedWatch::start(watches)))),
        }
    }

    async fn subscribe(
        &self,
        collection: &str,
        subscription: &PushSubscription,
    ) -> Result<Option<PushRegistration>, CollectionError> {
        self.owner(collection).await?;
        let side = self
            .owners
            .lock()
            .expect("owners lock")
            .get(collection)
            .copied()
            .ok_or(CollectionError::Gone)?;
        let registration = self
            .source(side)
            .subscribe(collection, subscription)
            .await?;
        if let Some(registration) = &registration {
            self.registrations
                .lock()
                .expect("registrations lock")
                .insert(registration.url.clone(), side);
        }
        Ok(registration)
    }

    /// Ends the registration with the source that made it; one this value did not make is
    /// handed to both sources.
    async fn unsubscribe(&self, registration: &PushRegistration) -> Result<(), CollectionError> {
        let side = self
            .registrations
            .lock()
            .expect("registrations lock")
            .remove(&registration.url);
        match side {
            Some(side) => self.source(side).unsubscribe(registration).await,
            None => {
                self.contacts.unsubscribe(registration).await?;
                self.calendars.unsubscribe(registration).await
            }
        }
    }
}

struct MergedWatch {
    notices: mpsc::UnboundedReceiver<Result<CollectionsChanged, CollectionError>>,
    tasks: Vec<JoinHandle<()>>,
}

impl MergedWatch {
    fn start(watches: Vec<Box<dyn CollectionWatch>>) -> Self {
        let (tx, notices) = mpsc::unbounded_channel();
        let tasks = watches
            .into_iter()
            .map(|mut watch| {
                let tx = tx.clone();
                tokio::spawn(async move {
                    loop {
                        let notice = watch.next().await;
                        let failed = notice.is_err();
                        if tx.send(notice).is_err() || failed {
                            watch.close().await;
                            return;
                        }
                    }
                })
            })
            .collect();
        Self { notices, tasks }
    }
}

impl Drop for MergedWatch {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[async_trait]
impl CollectionWatch for MergedWatch {
    async fn next(&mut self) -> Result<CollectionsChanged, CollectionError> {
        let mut changed = self
            .notices
            .recv()
            .await
            .ok_or_else(|| CollectionError::Connection("the watch ended".into()))??;
        while let Ok(more) = self.notices.try_recv() {
            changed = match (changed.collections, more?.collections) {
                (Some(mut a), Some(b)) => {
                    for id in b {
                        if !a.contains(&id) {
                            a.push(id);
                        }
                    }
                    CollectionsChanged {
                        collections: Some(a),
                    }
                }
                _ => CollectionsChanged { collections: None },
            };
        }
        Ok(changed)
    }

    async fn close(self: Box<Self>) {}
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use holtmail_collection::testing::{FakeCollections, Remote, conformance, watch_conformance};

    use super::*;

    /// The two fakes behind one [`Remote`], routed by collection id.
    struct Both(FakeCollections, FakeCollections);

    impl Both {
        fn of(&self, collection: &str) -> &FakeCollections {
            if collection.starts_with("/books/") {
                &self.0
            } else {
                &self.1
            }
        }
    }

    #[async_trait]
    impl Remote for Both {
        async fn put(&self, collection: &str, name: &str, raw: &[u8]) {
            self.of(collection).put_item(collection, name, raw);
        }

        async fn delete(&self, collection: &str, name: &str) {
            self.of(collection).delete_item(collection, name);
        }

        async fn expire_tokens(&self) {
            self.0.expire();
            self.1.expire();
        }
    }

    fn pair() -> (Both, MergedCollections) {
        let jmap = FakeCollections::new();
        jmap.add_collection("/books/main/", CollectionKind::Contacts, "Contacts", None);
        jmap.add_collection(
            "/jmap-calendars/c/",
            CollectionKind::Calendar,
            "Ignored",
            None,
        );
        let dav = FakeCollections::new();
        dav.add_collection("/books/dav/", CollectionKind::Contacts, "Ignored", None);
        dav.add_collection("/cal/work/", CollectionKind::Calendar, "Work", None);
        let merged = MergedCollections::new(Box::new(jmap.clone()), Box::new(dav.clone()));
        (Both(jmap, dav), merged)
    }

    #[tokio::test]
    async fn keeps_the_contract() {
        let (both, merged) = pair();
        let ids: Vec<String> = merged
            .collections()
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(ids, vec!["/books/main/", "/cal/work/"]);
        conformance(&merged, &both).await;
        assert!(
            both.0
                .fetches()
                .iter()
                .flatten()
                .all(|id| id.starts_with("/books/main/"))
        );
        assert!(
            both.1
                .fetches()
                .iter()
                .flatten()
                .all(|id| id.starts_with("/cal/work/"))
        );
    }

    #[tokio::test]
    async fn unknown_ids_are_gone_and_failures_fail_the_listing() {
        let (both, merged) = pair();
        assert!(matches!(
            merged.list("/books/dav/").await,
            Err(CollectionError::Gone)
        ));
        both.1.reject_sign_in(true);
        assert!(matches!(
            merged.collections().await,
            Err(CollectionError::Auth(_))
        ));
    }

    #[tokio::test]
    async fn watches_merge_and_one_side_alone_is_passed_through() {
        let (both, merged) = pair();
        assert!(merged.watch().await.unwrap().is_none());
        both.0.enable_watch();
        watch_conformance(&merged, &both).await;
        both.1.enable_watch();
        let mut watch = merged.watch().await.unwrap().unwrap();
        both.1.put_item("/cal/work/", "a.ics", b"x");
        let changed = tokio::time::timeout(Duration::from_secs(5), watch.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(changed.collections, Some(vec!["/cal/work/".to_owned()]));
        both.1.break_watches(Some("cut"));
        let ended = tokio::time::timeout(Duration::from_secs(5), watch.next())
            .await
            .unwrap();
        assert!(matches!(ended, Err(CollectionError::Connection(_))));
    }

    #[tokio::test]
    async fn subscriptions_go_to_the_collections_source() {
        let (both, merged) = pair();
        both.1.enable_push();
        let sub = PushSubscription {
            endpoint: "https://holtmail.example.com/api/push/dav/x".into(),
            public_key: vec![4; 65],
            auth_secret: vec![0; 16],
            expires: None,
        };
        assert_eq!(merged.subscribe("/books/main/", &sub).await.unwrap(), None);
        let registration = merged.subscribe("/cal/work/", &sub).await.unwrap().unwrap();
        assert_eq!(both.1.registrations().len(), 1);
        merged.unsubscribe(&registration).await.unwrap();
        assert!(both.1.registrations().is_empty());
    }
}
