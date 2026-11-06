//! Conformance suite every [`Store`] implementation runs in its own tests.

use std::sync::Arc;

use holtmail_blob::BlobId;
use holtmail_core::{
    AccountId, Address, ArchiveId, CollectionId, EventGroupId, ItemId, MessageId, PersonId,
    PersonOverrideId, Timestamp, UserId,
};
use serde_json::json;

use crate::*;

/// Checks that `store` keeps the [`Store`] contract. Panics on the first violation.
///
/// `store` must be empty when the suite starts.
pub async fn conformance(store: Arc<dyn Store>) {
    archives_and_accounts(&*store).await;
    folders(&*store).await;
    insert_is_idempotent(&*store).await;
    threads_merge_in_any_order(&*store).await;
    lists_page_newest_first(&*store).await;
    locations_follow_the_server(&*store).await;
    extras_and_unread(&*store).await;
    flag_changes(&*store).await;
    labels(&*store).await;
    sync_state(&*store).await;
    users(&*store).await;
    sessions(&*store).await;
    removal(&*store).await;
    user_changes_and_grants(&*store).await;
    remote_copies(&*store).await;
    settings(&*store).await;
    derived(&*store).await;
    inventory(&*store).await;
    tasks(&*store).await;
    last_runs(&*store).await;
    collections(&*store).await;
    item_versions(&*store).await;
    item_lists(&*store).await;
    pim_removal(&*store).await;
    mail_versions(&*store).await;
    collection_sync(&*store).await;
    people(&*store).await;
    person_overrides(&*store).await;
    calendar(&*store).await;
    ledger(&*store).await;
    deletions(&*store).await;
    webhooks(&*store).await;
}

fn pim_index(title: Option<&str>, starts: Option<i64>) -> PimIndex {
    PimIndex {
        version: 0,
        readable: true,
        title: title.map(Into::into),
        starts_at: starts.map(Timestamp::from_secs),
        ends_at: starts.map(|s| Timestamp::from_secs(s + 1800)),
        all_day: false,
        time_zone: Some("Europe/Berlin".into()),
        recurring: false,
        sequence: Some(1),
        uid: title.map(|t| format!("{t}@example.com")),
        organizer: None,
        emails: vec!["ada@example.com".into()],
        phones: vec!["+15550100".into()],
        data: json!({ "type": "contact", "full_name": title }),
    }
}

fn item_version(
    collection: CollectionId,
    remote: &str,
    bytes: &str,
    kind: ItemKind,
    index: PimIndex,
) -> NewItemVersion {
    NewItemVersion {
        collection_id: collection,
        remote_id: remote.into(),
        kind,
        key: Some(format!("{remote}-key")),
        blob_id: BlobId::of(bytes.as_bytes()),
        content_type: "text/vcard".into(),
        etag: Some(format!("\"{bytes}\"")),
        seen_at: Timestamp::from_secs(1_700_000_000),
        photo_blob_id: None,
        index,
    }
}

async fn book(store: &dyn Store, archive: ArchiveId, remote: &str) -> CollectionId {
    store
        .upsert_collection(NewCollection {
            archive_id: archive,
            account_id: None,
            kind: CollectionKind::Contacts,
            remote_id: remote.into(),
            name: remote.into(),
            color: None,
        })
        .await
        .unwrap()
        .id
}

async fn collections(store: &dyn Store) {
    let archive = store.create_archive("Address books").await.unwrap().id;
    let elsewhere = store.create_archive("Elsewhere").await.unwrap().id;
    let account = import_account(store, archive).await;
    let foreign = import_account(store, elsewhere).await;
    let new = |remote: &str, name: &str, kind| NewCollection {
        archive_id: archive,
        account_id: Some(account.id),
        kind,
        remote_id: remote.into(),
        name: name.into(),
        color: None,
    };
    let people = store
        .upsert_collection(new("people.vcf", "People", CollectionKind::Contacts))
        .await
        .unwrap();
    assert_eq!(people.archive_id, archive);
    assert_eq!(people.account_id, Some(account.id));
    assert_eq!(people.removed_at, None);
    let mut renamed = new("people.vcf", "Friends", CollectionKind::Contacts);
    renamed.color = Some("#336699".into());
    let again = store.upsert_collection(renamed).await.unwrap();
    assert_eq!(again.id, people.id, "upsert is idempotent per remote id");
    assert_eq!(again.name, "Friends");
    assert_eq!(again.color.as_deref(), Some("#336699"));
    let team = store
        .upsert_collection(new("team.ics", "Team", CollectionKind::Calendar))
        .await
        .unwrap();
    assert_ne!(team.id, people.id);
    assert_eq!(
        store.get_collection(people.id).await.unwrap(),
        Some(again.clone())
    );
    assert_eq!(
        store.get_collection(CollectionId::new()).await.unwrap(),
        None
    );

    let listed = store.list_collections(archive).await.unwrap();
    let names: Vec<_> = listed.iter().map(|c| c.collection.name.as_str()).collect();
    assert_eq!(names, ["Team", "Friends"], "by kind, then name");
    assert_eq!(listed[1].items, 0);
    assert!(store.list_collections(elsewhere).await.unwrap().is_empty());

    store
        .set_sync_token(team.id, Some("token-1"))
        .await
        .unwrap();
    assert_eq!(
        store
            .get_collection(team.id)
            .await
            .unwrap()
            .unwrap()
            .sync_token
            .as_deref(),
        Some("token-1")
    );
    assert!(matches!(
        store.set_sync_token(CollectionId::new(), None).await,
        Err(StoreError::NotFound(_))
    ));

    let mut missing = new("x.vcf", "X", CollectionKind::Contacts);
    missing.archive_id = ArchiveId::new();
    assert!(matches!(
        store.upsert_collection(missing).await,
        Err(StoreError::NotFound(_))
    ));
    let mut stranger = new("x.vcf", "X", CollectionKind::Contacts);
    stranger.account_id = Some(foreign.id);
    assert!(matches!(
        store.upsert_collection(stranger).await,
        Err(StoreError::Invalid(_))
    ));
    let mut unknown = new("x.vcf", "X", CollectionKind::Contacts);
    unknown.account_id = Some(AccountId::new());
    assert!(matches!(
        store.upsert_collection(unknown).await,
        Err(StoreError::NotFound(_))
    ));

    store.delete_account(account.id).await.unwrap();
    let kept = store.get_collection(people.id).await.unwrap().unwrap();
    assert_eq!(
        kept.account_id, None,
        "a removed account leaves its collections"
    );
    store.delete_archive(archive).await.unwrap();
    store.delete_archive(elsewhere).await.unwrap();
}

async fn item_versions(store: &dyn Store) {
    let archive = store.create_archive("Versions").await.unwrap().id;
    let people = book(store, archive, "people.vcf").await;
    let first = store
        .record_version(item_version(
            people,
            "ada",
            "ada v1",
            ItemKind::Contact,
            pim_index(Some("Ada"), None),
        ))
        .await
        .unwrap();
    assert!(first.created_item && first.new_version && !first.restored);
    assert_eq!(first.number, 1);

    let same = store
        .record_version(item_version(
            people,
            "ada",
            "ada v1",
            ItemKind::Contact,
            pim_index(Some("Ignored"), None),
        ))
        .await
        .unwrap();
    assert_eq!(same.item_id, first.item_id);
    assert!(
        !same.created_item && !same.new_version,
        "same bytes, no version"
    );
    assert_eq!(same.number, 1);

    let mut photo = item_version(
        people,
        "ada",
        "ada v2",
        ItemKind::Contact,
        pim_index(Some("Ada Quill"), None),
    );
    photo.photo_blob_id = Some(BlobId::of(b"ada's picture"));
    photo.key = Some("ada-new-key".into());
    photo.seen_at = Timestamp::from_secs(1_700_000_500);
    let second = store.record_version(photo).await.unwrap();
    assert!(second.new_version && !second.created_item);
    assert_eq!(second.number, 2);

    let versions = store.item_versions(first.item_id).await.unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].number, 1);
    assert_eq!(versions[0].blob_id, BlobId::of(b"ada v1"));
    assert_eq!(versions[0].etag.as_deref(), Some("\"ada v1\""));
    assert_eq!(versions[0].content_type, "text/vcard");
    assert_eq!(
        versions[1].photo_blob_id,
        Some(BlobId::of(b"ada's picture"))
    );
    assert_eq!(versions[1].seen_at, Timestamp::from_secs(1_700_000_500));

    let item = store.get_item(first.item_id).await.unwrap().unwrap();
    assert_eq!(item.item.versions, 2);
    assert_eq!(item.item.key.as_deref(), Some("ada-new-key"));
    assert_eq!(item.item.archive_id, archive);
    assert_eq!(item.item.collection_id, people);
    assert_eq!(item.item.first_seen_at, Timestamp::from_secs(1_700_000_000));
    let index = item.index.unwrap();
    assert_eq!(index.version, 2, "the index follows the latest version");
    assert_eq!(index.title.as_deref(), Some("Ada Quill"));
    assert_eq!(index.emails, ["ada@example.com"]);
    assert_eq!(index.phones, ["+15550100"]);
    assert_eq!(index.data["type"], "contact");
    assert_eq!(index.time_zone.as_deref(), Some("Europe/Berlin"));

    let back = store
        .record_version(item_version(
            people,
            "ada",
            "ada v1",
            ItemKind::Contact,
            pim_index(Some("Ada"), None),
        ))
        .await
        .unwrap();
    assert!(
        back.new_version,
        "going back to older bytes is a new version"
    );
    assert_eq!(back.number, 3);

    let mut stale = pim_index(Some("Stale"), None);
    stale.version = 2;
    store.set_pim_index(first.item_id, &stale).await.unwrap();
    assert_eq!(
        store
            .get_item(first.item_id)
            .await
            .unwrap()
            .unwrap()
            .index
            .unwrap()
            .title
            .as_deref(),
        Some("Ada"),
        "fields of an older version are ignored"
    );
    let mut rebuilt = pim_index(Some("Rebuilt"), None);
    rebuilt.version = 3;
    store.set_pim_index(first.item_id, &rebuilt).await.unwrap();
    assert_eq!(
        store
            .get_item(first.item_id)
            .await
            .unwrap()
            .unwrap()
            .index
            .unwrap()
            .title
            .as_deref(),
        Some("Rebuilt")
    );
    assert!(matches!(
        store.set_pim_index(ItemId::new(), &rebuilt).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        store
            .record_version(item_version(
                CollectionId::new(),
                "x",
                "x",
                ItemKind::Contact,
                pim_index(None, None)
            ))
            .await,
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(store.get_item(ItemId::new()).await.unwrap(), None);
    assert!(store.item_versions(ItemId::new()).await.unwrap().is_empty());
    store.delete_archive(archive).await.unwrap();
}

async fn item_lists(store: &dyn Store) {
    let archive = store.create_archive("Lists").await.unwrap().id;
    let people = book(store, archive, "people.vcf").await;
    let more = book(store, archive, "more.vcf").await;
    let team = store
        .upsert_collection(NewCollection {
            archive_id: archive,
            account_id: None,
            kind: CollectionKind::Calendar,
            remote_id: "team.ics".into(),
            name: "Team".into(),
            color: None,
        })
        .await
        .unwrap()
        .id;
    for (collection, remote, title) in [
        (people, "cleo", None),
        (people, "bo", Some("bo Linden")),
        (more, "ada", Some("Ada Quill")),
        (people, "dee", Some("Dee")),
    ] {
        store
            .record_version(item_version(
                collection,
                remote,
                remote,
                ItemKind::Contact,
                pim_index(title, None),
            ))
            .await
            .unwrap();
    }
    for (remote, start) in [
        ("late", Some(1_700_100_000)),
        ("none", None),
        ("early", Some(1_700_000_000)),
    ] {
        store
            .record_version(item_version(
                team,
                remote,
                remote,
                ItemKind::Event,
                pim_index(Some(remote), start),
            ))
            .await
            .unwrap();
    }
    let query = |kind, order, after, limit| ItemQuery {
        archive_id: archive,
        kind: Some(kind),
        collection_id: None,
        removed: RemovedFilter::Hide,
        order,
        after,
        limit,
    };
    let mut seen = Vec::new();
    let mut after = None;
    loop {
        let page = store
            .list_items(&query(ItemKind::Contact, ItemOrder::Title, after, 2))
            .await
            .unwrap();
        assert!(page.items.len() <= 2);
        seen.extend(page.items.iter().map(|s| s.item.remote_id.clone()));
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    assert_eq!(
        seen,
        ["ada", "bo", "dee", "cleo"],
        "by title ignoring case, untitled last"
    );

    let events = store
        .list_items(&query(ItemKind::Event, ItemOrder::Start, None, 10))
        .await
        .unwrap();
    let order: Vec<_> = events
        .items
        .iter()
        .map(|s| s.item.remote_id.as_str())
        .collect();
    assert_eq!(order, ["early", "late", "none"], "by start, no start last");
    let first = store
        .list_items(&query(ItemKind::Event, ItemOrder::Start, None, 1))
        .await
        .unwrap();
    let rest = store
        .list_items(&query(ItemKind::Event, ItemOrder::Start, first.next, 10))
        .await
        .unwrap();
    let order: Vec<_> = rest
        .items
        .iter()
        .map(|s| s.item.remote_id.as_str())
        .collect();
    assert_eq!(order, ["late", "none"]);

    let mut only_people = query(ItemKind::Contact, ItemOrder::Title, None, 10);
    only_people.collection_id = Some(people);
    let listed = store.list_items(&only_people).await.unwrap();
    assert_eq!(listed.items.len(), 3);
    assert!(listed.next.is_none());
    let mut everything = query(ItemKind::Contact, ItemOrder::Title, None, 10);
    everything.kind = None;
    assert_eq!(store.list_items(&everything).await.unwrap().items.len(), 7);

    assert_eq!(
        store
            .mark_items_removed(
                people,
                &["bo".into(), "missing".into()],
                Timestamp::from_secs(1_700_000_900)
            )
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .mark_items_removed(people, &["bo".into()], Timestamp::from_secs(1_700_001_000))
            .await
            .unwrap(),
        0,
        "already removed"
    );
    assert_eq!(
        store.live_remote_ids(people).await.unwrap(),
        ["cleo", "dee"]
    );
    let removed = |filter| {
        let mut q = query(ItemKind::Contact, ItemOrder::Title, None, 10);
        q.removed = filter;
        q
    };
    let names = |page: Page<ItemSummary>| -> Vec<String> {
        page.items.into_iter().map(|s| s.item.remote_id).collect()
    };
    assert_eq!(
        names(
            store
                .list_items(&removed(RemovedFilter::Hide))
                .await
                .unwrap()
        ),
        ["ada", "dee", "cleo"]
    );
    let gone = store
        .list_items(&removed(RemovedFilter::Only))
        .await
        .unwrap();
    assert_eq!(
        gone.items[0].item.removed_at,
        Some(Timestamp::from_secs(1_700_000_900)),
        "the first removal time is kept"
    );
    assert_eq!(names(gone), ["bo"]);
    assert_eq!(
        store
            .list_items(&removed(RemovedFilter::Include))
            .await
            .unwrap()
            .items
            .len(),
        4
    );
    let summary = store.list_collections(archive).await.unwrap();
    let people_summary = summary.iter().find(|c| c.collection.id == people).unwrap();
    assert_eq!((people_summary.items, people_summary.removed_items), (2, 1));

    let back = store
        .record_version(item_version(
            people,
            "bo",
            "bo",
            ItemKind::Contact,
            pim_index(Some("bo Linden"), None),
        ))
        .await
        .unwrap();
    assert!(back.restored && !back.new_version, "seen again, same bytes");
    assert_eq!(store.live_remote_ids(people).await.unwrap().len(), 3);

    store
        .mark_collection_removed(team, Timestamp::from_secs(1_700_002_000))
        .await
        .unwrap();
    let removed_team = store.get_collection(team).await.unwrap().unwrap();
    assert_eq!(
        removed_team.removed_at,
        Some(Timestamp::from_secs(1_700_002_000))
    );
    assert!(store.live_remote_ids(team).await.unwrap().is_empty());
    assert!(matches!(
        store
            .mark_collection_removed(CollectionId::new(), Timestamp::from_secs(1))
            .await,
        Err(StoreError::NotFound(_))
    ));

    let mut ids = Vec::new();
    let mut after = None;
    loop {
        let page = store.item_ids(after, 3).await.unwrap();
        let done = page.len() < 3;
        after = page.last().copied();
        ids.extend(page);
        if done {
            break;
        }
    }
    assert!(ids.windows(2).all(|w| w[0] < w[1]));
    assert!(ids.len() >= 7);

    let bad = query(
        ItemKind::Contact,
        ItemOrder::Title,
        Some(PageCursor("garbage".into())),
        10,
    );
    assert!(matches!(
        store.list_items(&bad).await,
        Err(StoreError::Invalid(_))
    ));
    store.delete_archive(archive).await.unwrap();
}

async fn pim_removal(store: &dyn Store) {
    let gone = store.create_archive("Gone contacts").await.unwrap().id;
    let kept = store.create_archive("Kept contacts").await.unwrap().id;
    let old = book(store, gone, "people.vcf").await;
    let other = book(store, kept, "people.vcf").await;
    let mut ada = item_version(
        old,
        "ada",
        "shared card",
        ItemKind::Contact,
        pim_index(Some("Ada"), None),
    );
    ada.photo_blob_id = Some(BlobId::of(b"only in the old archive"));
    let ada = store.record_version(ada).await.unwrap().item_id;
    store
        .record_version(item_version(
            old,
            "ada",
            "own card",
            ItemKind::Contact,
            pim_index(Some("Ada"), None),
        ))
        .await
        .unwrap();
    store
        .record_version(item_version(
            other,
            "ada",
            "shared card",
            ItemKind::Contact,
            pim_index(Some("Ada"), None),
        ))
        .await
        .unwrap();

    let referenced = store.referenced_blobs(None, 100_000).await.unwrap();
    for blob in [
        BlobId::of(b"shared card"),
        BlobId::of(b"own card"),
        BlobId::of(b"only in the old archive"),
    ] {
        assert!(referenced.contains(&blob), "item blobs are referenced");
    }

    let mut orphans = vec![
        BlobId::of(b"own card"),
        BlobId::of(b"only in the old archive"),
    ];
    orphans.sort();
    assert_eq!(
        store.delete_archive(gone).await.unwrap(),
        orphans,
        "a version blob another archive still uses is not an orphan"
    );
    assert_eq!(store.get_collection(old).await.unwrap(), None);
    assert_eq!(store.get_item(ada).await.unwrap(), None);
    assert!(store.item_versions(ada).await.unwrap().is_empty());
    assert_eq!(store.list_collections(kept).await.unwrap().len(), 1);
    store.delete_archive(kept).await.unwrap();
}

async fn collection_sync(store: &dyn Store) {
    let archive = store.create_archive("Synced books").await.unwrap().id;
    let people = book(store, archive, "/dav/people/").await;
    let fresh = store.get_collection(people).await.unwrap().unwrap();
    assert_eq!((fresh.ctag, fresh.synced_at), (None, None));

    store
        .set_collection_synced(
            people,
            Some("sync-1"),
            Some("ctag-1"),
            Timestamp::from_secs(1_700_000_100),
        )
        .await
        .unwrap();
    let synced = store.get_collection(people).await.unwrap().unwrap();
    assert_eq!(synced.sync_token.as_deref(), Some("sync-1"));
    assert_eq!(synced.ctag.as_deref(), Some("ctag-1"));
    assert_eq!(synced.synced_at, Some(Timestamp::from_secs(1_700_000_100)));
    store
        .set_collection_synced(people, None, None, Timestamp::from_secs(1_700_000_200))
        .await
        .unwrap();
    let cleared = store.get_collection(people).await.unwrap().unwrap();
    assert_eq!((cleared.sync_token, cleared.ctag), (None, None));
    assert!(matches!(
        store
            .set_collection_synced(CollectionId::new(), None, None, Timestamp::now())
            .await,
        Err(StoreError::NotFound(_))
    ));

    for (remote, bytes) in [("b.vcf", "bea"), ("a.vcf", "ada")] {
        store
            .record_version(item_version(
                people,
                remote,
                bytes,
                ItemKind::Contact,
                pim_index(Some(bytes), None),
            ))
            .await
            .unwrap();
    }
    assert_eq!(
        store.item_etags(people).await.unwrap(),
        [
            ("a.vcf".to_owned(), Some("\"ada\"".to_owned())),
            ("b.vcf".to_owned(), Some("\"bea\"".to_owned())),
        ],
        "sorted by remote id"
    );
    let mut touched = item_version(
        people,
        "a.vcf",
        "ada",
        ItemKind::Contact,
        pim_index(Some("ada"), None),
    );
    touched.etag = Some("\"ada-2\"".into());
    let same = store.record_version(touched).await.unwrap();
    assert!(!same.new_version);
    assert_eq!(
        store.item_etags(people).await.unwrap()[0].1.as_deref(),
        Some("\"ada-2\""),
        "the same bytes under a new tag keep one version but the new tag"
    );
    assert_eq!(
        store.item_versions(same.item_id).await.unwrap()[0]
            .etag
            .as_deref(),
        Some("\"ada\""),
        "a stored version keeps the tag it was seen with"
    );
    store
        .mark_items_removed(people, &["b.vcf".into()], Timestamp::now())
        .await
        .unwrap();
    let live: Vec<_> = store
        .item_etags(people)
        .await
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(live, ["a.vcf"], "removed items are left out");
    assert!(
        store
            .item_etags(CollectionId::new())
            .await
            .unwrap()
            .is_empty()
    );
    store.delete_archive(archive).await.unwrap();
}

fn mail_version(
    collection: CollectionId,
    message: MessageId,
    bytes: &str,
    method: &str,
    sequence: i64,
    sent_secs: i64,
) -> NewMailVersion {
    let mut version = item_version(
        collection,
        "plan@example.com",
        bytes,
        ItemKind::Event,
        pim_index(Some(bytes), Some(sent_secs)),
    );
    version.content_type = "text/calendar".into();
    version.etag = None;
    NewMailVersion {
        version,
        message_id: message,
        mail: MailVersionInfo {
            method: Some(method.into()),
            sequence: Some(sequence),
            dtstamp: None,
            sent_at: Some(Timestamp::from_secs(sent_secs)),
        },
    }
}

async fn mail_versions(store: &dyn Store) {
    let archive = store.create_archive("Invitations").await.unwrap().id;
    let other = store.create_archive("Other mail").await.unwrap().id;
    let calendar = store
        .upsert_collection(NewCollection {
            archive_id: archive,
            account_id: None,
            kind: CollectionKind::Calendar,
            remote_id: "mail".into(),
            name: "From mail".into(),
            color: None,
        })
        .await
        .unwrap()
        .id;
    let mut ids = Vec::new();
    for n in 0..5 {
        let message = store
            .insert_message(message(archive, 9_100 + n, 1_700_000_000 + i64::from(n)))
            .await
            .unwrap();
        ids.push(message.message_id);
    }
    let foreign = store
        .insert_message(message(other, 9_199, 1_700_000_000))
        .await
        .unwrap()
        .message_id;

    let cancel = mail_version(calendar, ids[3], "cancel", "CANCEL", 2, 1_700_000_300);
    let first = store.record_mail_version(cancel.clone()).await.unwrap();
    assert!(first.created_item && first.new_version);
    assert_eq!(first.number, 1);
    let item = first.item_id;
    let request = mail_version(calendar, ids[0], "request", "REQUEST", 0, 1_700_000_000);
    let added = store.record_mail_version(request).await.unwrap();
    assert!(!added.created_item && added.new_version);
    assert_eq!(added.number, 2, "the latest number after the call");
    let moved = mail_version(calendar, ids[1], "moved", "REQUEST", 1, 1_700_000_100);
    store.record_mail_version(moved).await.unwrap();
    let reply = mail_version(calendar, ids[2], "reply", "REPLY", 1, 1_700_000_200);
    store.record_mail_version(reply).await.unwrap();

    let versions = store.item_versions(item).await.unwrap();
    let order: Vec<_> = versions
        .iter()
        .map(|v| {
            (
                v.number,
                v.blob_id,
                v.mail.as_ref().and_then(|m| m.method.clone()),
            )
        })
        .collect();
    assert_eq!(
        order,
        [
            (1, BlobId::of(b"request"), Some("REQUEST".into())),
            (2, BlobId::of(b"moved"), Some("REQUEST".into())),
            (3, BlobId::of(b"reply"), Some("REPLY".into())),
            (4, BlobId::of(b"cancel"), Some("CANCEL".into())),
        ],
        "versions follow SEQUENCE and the message date, not the order they were recorded in"
    );
    assert_eq!(versions[1].mail.as_ref().unwrap().sequence, Some(1));
    assert_eq!(
        versions[1].mail.as_ref().unwrap().sent_at,
        Some(Timestamp::from_secs(1_700_000_100))
    );
    let summary = store.get_item(item).await.unwrap().unwrap();
    assert_eq!(summary.item.versions, 4);
    assert_eq!(summary.item.kind, ItemKind::Event);

    let again = mail_version(calendar, ids[4], "moved", "REQUEST", 1, 1_700_000_400);
    let same = store.record_mail_version(again.clone()).await.unwrap();
    assert!(!same.new_version && !same.created_item);
    assert_eq!(same.number, 4);
    store.record_mail_version(again).await.unwrap();
    assert_eq!(store.item_versions(item).await.unwrap().len(), 4);

    let sources: Vec<_> = store
        .version_messages(item)
        .await
        .unwrap()
        .into_iter()
        .map(|s| (s.number, s.message_id))
        .collect();
    let mut expected = vec![
        (1, ids[0]),
        (2, ids[1]),
        (2, ids[4]),
        (3, ids[2]),
        (4, ids[3]),
    ];
    expected.sort();
    assert_eq!(sources, expected);
    assert_eq!(
        store.message_versions(ids[4]).await.unwrap(),
        [VersionSource {
            item_id: item,
            number: 2,
            message_id: ids[4],
        }]
    );
    assert!(store.message_versions(foreign).await.unwrap().is_empty());

    let mut wrong = mail_version(calendar, foreign, "foreign", "REQUEST", 3, 1_700_000_500);
    assert!(matches!(
        store.record_mail_version(wrong.clone()).await,
        Err(StoreError::Invalid(_))
    ));
    wrong.message_id = MessageId::new();
    assert!(matches!(
        store.record_mail_version(wrong.clone()).await,
        Err(StoreError::NotFound(_))
    ));
    wrong.message_id = ids[0];
    wrong.version.collection_id = CollectionId::new();
    assert!(matches!(
        store.record_mail_version(wrong).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(
        store
            .version_messages(ItemId::new())
            .await
            .unwrap()
            .is_empty()
    );

    store.delete_archive(archive).await.unwrap();
    assert!(store.message_versions(ids[0]).await.unwrap().is_empty());
    assert!(store.version_messages(item).await.unwrap().is_empty());
    store.delete_archive(other).await.unwrap();
}

fn task(id: &str, started: i64, state: TaskState) -> TaskRecord {
    TaskRecord {
        id: id.into(),
        kind: TaskKind::RemoteImages,
        archive_id: Some(ArchiveId::new()),
        account_id: None,
        title: "News".into(),
        state,
        started_at: Timestamp::from_secs(started),
        finished_at: (state != TaskState::Running).then(|| Timestamp::from_secs(started + 1)),
        done: 3,
        total: Some(10),
        summary: "3 of 10".into(),
        cancellable: true,
        job: Some("older".into()),
    }
}

fn line(n: usize) -> TaskLine {
    TaskLine {
        at: Timestamp::from_millis(n as i64),
        level: if n.is_multiple_of(2) {
            TaskLevel::Info
        } else {
            TaskLevel::Warning
        },
        text: format!("line {n}"),
    }
}

async fn tasks(store: &dyn Store) {
    assert!(store.recent_tasks(10).await.unwrap().is_empty());
    assert!(matches!(
        store.append_task_log("missing", &[line(0)]).await,
        Err(StoreError::NotFound(_))
    ));

    let mut running = task("a", 100, TaskState::Running);
    store.save_task(&running).await.unwrap();
    store.save_task(&running).await.unwrap();
    store
        .append_task_log("a", &[line(0), line(1)])
        .await
        .unwrap();
    running.done = 7;
    running.summary = "7 of 10".into();
    store.save_task(&running).await.unwrap();
    let [stored] = store.recent_tasks(10).await.unwrap().try_into().unwrap();
    assert_eq!(stored.record, running);
    assert_eq!(
        stored.log,
        vec![line(0), line(1)],
        "saving the task lost its log"
    );

    let total = TASK_LOG_LINES + 120;
    let all: Vec<TaskLine> = (2..total).map(line).collect();
    for chunk in all.chunks(77) {
        store.append_task_log("a", chunk).await.unwrap();
    }
    let log = store.recent_tasks(1).await.unwrap().remove(0).log;
    assert_eq!(log.len(), TASK_LOG_LINES);
    let expected: Vec<TaskLine> = (0..TASK_LOG_HEAD)
        .chain(total - (TASK_LOG_LINES - TASK_LOG_HEAD)..total)
        .map(line)
        .collect();
    assert_eq!(log, expected, "the log does not keep its start and its end");

    for (n, state) in [
        TaskState::Done,
        TaskState::Failed,
        TaskState::Cancelled,
        TaskState::Done,
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("f{n}");
        store
            .save_task(&task(&id, 200 + n as i64, state))
            .await
            .unwrap();
        store.append_task_log(&id, &[line(n)]).await.unwrap();
    }
    let mut other = task("b", 50, TaskState::Running);
    other.kind = TaskKind::Sync;
    other.account_id = Some(AccountId::new());
    other.total = None;
    other.job = None;
    other.cancellable = false;
    store.save_task(&other).await.unwrap();

    let ids = |list: Vec<StoredTask>| list.into_iter().map(|t| t.record.id).collect::<Vec<_>>();
    assert_eq!(
        ids(store.recent_tasks(10).await.unwrap()),
        ["f3", "f2", "f1", "f0", "a", "b"]
    );
    assert_eq!(ids(store.recent_tasks(2).await.unwrap()), ["f3", "f2"]);

    assert_eq!(store.prune_tasks(2).await.unwrap(), 2);
    assert_eq!(
        ids(store.recent_tasks(10).await.unwrap()),
        ["f3", "f2", "a", "b"],
        "pruning removed a running task or kept an old finished one"
    );
    assert_eq!(store.prune_tasks(2).await.unwrap(), 0);

    let at = Timestamp::from_secs(900);
    let mut interrupted = store.interrupt_tasks(at).await.unwrap();
    interrupted.sort_by(|x, y| x.id.cmp(&y.id));
    assert_eq!(interrupted.len(), 2);
    for record in &interrupted {
        assert_eq!(record.state, TaskState::Interrupted);
        assert_eq!(record.finished_at, Some(at));
    }
    assert_eq!(
        interrupted[1],
        TaskRecord {
            state: TaskState::Interrupted,
            finished_at: Some(at),
            ..other
        }
    );
    assert!(store.interrupt_tasks(at).await.unwrap().is_empty());
    let after = store.recent_tasks(10).await.unwrap();
    assert!(after.iter().all(|t| t.record.state != TaskState::Running));
    assert_eq!(
        after[2].log.len(),
        TASK_LOG_LINES,
        "interrupting lost the log"
    );

    assert_eq!(store.prune_tasks(0).await.unwrap(), 4);
    assert!(store.recent_tasks(10).await.unwrap().is_empty());
    assert!(
        matches!(
            store.append_task_log("a", &[line(0)]).await,
            Err(StoreError::NotFound(_))
        ),
        "a removed task kept its log"
    );
}

async fn last_runs(store: &dyn Store) {
    let archive = archive(store).await;
    let first = import_account(store, archive).await;
    let second = import_account(store, archive).await;
    assert!(
        !store.last_runs().await.unwrap().contains_key(&first.id),
        "a new account has a last run"
    );
    let ok = AccountRun {
        finished_at: Timestamp::from_secs(10),
        new_messages: 4,
        error: None,
    };
    let failed = AccountRun {
        finished_at: Timestamp::from_secs(20),
        new_messages: 0,
        error: Some("Could not connect".into()),
    };
    store.set_last_run(first.id, &failed).await.unwrap();
    store.set_last_run(first.id, &ok).await.unwrap();
    store.set_last_run(second.id, &failed).await.unwrap();
    let runs = store.last_runs().await.unwrap();
    assert_eq!(runs.get(&first.id), Some(&ok));
    assert_eq!(runs.get(&second.id), Some(&failed));
    assert!(matches!(
        store.set_last_run(AccountId::new(), &ok).await,
        Err(StoreError::NotFound(_))
    ));

    store.delete_account(first.id).await.unwrap();
    let runs = store.last_runs().await.unwrap();
    assert!(
        !runs.contains_key(&first.id),
        "a removed account kept its run"
    );
    store.delete_archive(archive).await.unwrap();
    assert!(
        !store.last_runs().await.unwrap().contains_key(&second.id),
        "a removed archive kept its accounts' runs"
    );
}

async fn inventory(store: &dyn Store) {
    let archive = store.create_archive("Inventory").await.unwrap().id;
    let picture = BlobId::of(b"inventory: a picture");
    let thumb = BlobId::of(b"inventory: its thumbnail");
    let image = BlobId::of(b"inventory: a remote image");
    let mut new = message(archive, 9401, 1_700_000_000);
    new.attachments.push(Attachment {
        ordinal: 0,
        blob_id: picture,
        filename: Some("picture.png".into()),
        content_type: "image/png".into(),
        size: 20,
        content_id: None,
        inline: false,
    });
    let message_blob = new.blob_id;
    let inserted = store.insert_message(new).await.unwrap();
    let mut twin = message(archive, 9402, 1_700_000_100);
    twin.attachments.push(Attachment {
        ordinal: 0,
        blob_id: picture,
        filename: Some("same.png".into()),
        content_type: "image/png".into(),
        size: 20,
        content_id: None,
        inline: false,
    });
    store.insert_message(twin).await.unwrap();
    store
        .set_derived(&picture, DerivedKind::Thumbnail, &Derived::Blob(thumb))
        .await
        .unwrap();
    store
        .save_remote_copy(&RemoteCopy {
            key: "inventory-key".into(),
            url: "https://images.example.com/inventory.png".into(),
            blob_id: Some(image),
            content_type: Some("image/png".into()),
            size: 10,
            fetched_at: Timestamp::from_secs(1_700_000_000),
            error: None,
        })
        .await
        .unwrap();
    store
        .record_remote_scan(inserted.message_id, &["inventory-key".into()])
        .await
        .unwrap();

    let all = store.referenced_blobs(None, 100_000).await.unwrap();
    assert!(
        all.windows(2).all(|w| w[0] < w[1]),
        "referenced blobs are ascending and listed once"
    );
    for blob in [message_blob, picture, thumb, image] {
        assert!(all.contains(&blob), "{blob} is referenced");
    }

    let mut paged = Vec::new();
    let mut after = None;
    loop {
        let page = store.referenced_blobs(after.as_ref(), 2).await.unwrap();
        assert!(page.len() <= 2);
        let last = page.last().copied();
        let done = page.len() < 2;
        paged.extend(page);
        match last {
            Some(last) if !done => after = Some(last),
            _ => break,
        }
    }
    assert_eq!(paged, all, "paging visits every blob exactly once");
    assert!(
        store
            .referenced_blobs(all.last(), 10)
            .await
            .unwrap()
            .is_empty()
    );

    store.delete_archive(archive).await.unwrap();
    let left = store.referenced_blobs(None, 100_000).await.unwrap();
    for blob in [message_blob, picture, thumb, image] {
        assert!(!left.contains(&blob), "{blob} is no longer referenced");
    }
}

async fn settings(store: &dyn Store) {
    assert_eq!(store.setting("image_proxy").await.unwrap(), None);
    store
        .set_setting("image_proxy", Some("first"))
        .await
        .unwrap();
    store
        .set_setting("image_proxy", Some("second"))
        .await
        .unwrap();
    assert_eq!(
        store.setting("image_proxy").await.unwrap().as_deref(),
        Some("second")
    );
    store.set_setting("image_proxy", None).await.unwrap();
    store.set_setting("image_proxy", None).await.unwrap();
    assert_eq!(store.setting("image_proxy").await.unwrap(), None);
}

/// A message with invented headers; `n` makes its bytes unique.
pub fn message(archive: ArchiveId, n: u32, sent_secs: i64) -> NewMessage {
    NewMessage {
        archive_id: archive,
        blob_id: BlobId::of(format!("message {n}").as_bytes()),
        size: 100 + u64::from(n),
        rfc_message_id: Some(format!("m{n}@example.com")),
        parent_ids: Vec::new(),
        subject: Some(format!("Subject {n}")),
        from: vec![Address {
            name: Some("Ada".into()),
            email: "ada@example.com".into(),
        }],
        to: vec![Address {
            name: None,
            email: "bob@example.org".into(),
        }],
        cc: Vec::new(),
        bcc: Vec::new(),
        sent_at: Some(Timestamp::from_secs(sent_secs)),
        received_at: None,
        preview: format!("Preview {n}"),
        attachments: Vec::new(),
    }
}

async fn archive(store: &dyn Store) -> ArchiveId {
    store.create_archive("Test").await.unwrap().id
}

async fn archives_and_accounts(store: &dyn Store) {
    let first = store.create_archive("Personal").await.unwrap();
    let second = store.create_archive("Work").await.unwrap();
    assert_eq!(
        store.get_archive(first.id).await.unwrap(),
        Some(first.clone())
    );
    let names: Vec<_> = store
        .list_archives()
        .await
        .unwrap()
        .into_iter()
        .map(|a| a.name)
        .collect();
    assert_eq!(names, ["Personal", "Work"], "archives are not oldest first");
    assert_eq!(
        store.archive_stats(second.id).await.unwrap(),
        ArchiveStats::default()
    );

    let mut account = store
        .create_account(NewAccount {
            archive_id: first.id,
            kind: AccountKind::Imap,
            name: "Mail".into(),
            address: Some("ada@example.com".into()),
            settings: json!({ "host": "imap.example.com", "port": 993 }),
            sealed_secret: Some(vec![1, 2, 3]),
            enabled: true,
        })
        .await
        .unwrap();
    assert_eq!(
        store.get_account(account.id).await.unwrap(),
        Some(account.clone())
    );
    assert_eq!(
        store
            .get_account(account.id)
            .await
            .unwrap()
            .unwrap()
            .sealed_secret,
        Some(vec![1, 2, 3]),
        "sealed secret was not kept"
    );

    account.name = "Old mail".into();
    account.enabled = false;
    account.sealed_secret = None;
    store.update_account(&account).await.unwrap();
    assert_eq!(store.list_accounts(first.id).await.unwrap(), vec![account]);
    assert!(store.list_accounts(second.id).await.unwrap().is_empty());

    let missing = store
        .create_account(NewAccount {
            archive_id: ArchiveId::new(),
            kind: AccountKind::Import,
            name: "Nowhere".into(),
            address: None,
            settings: json!({}),
            sealed_secret: None,
            enabled: false,
        })
        .await;
    assert!(
        matches!(missing, Err(StoreError::NotFound(_))),
        "account in a missing archive"
    );
}

async fn import_account(store: &dyn Store, archive: ArchiveId) -> Account {
    store
        .create_account(NewAccount {
            archive_id: archive,
            kind: AccountKind::Import,
            name: "Import".into(),
            address: None,
            settings: json!({}),
            sealed_secret: None,
            enabled: false,
        })
        .await
        .unwrap()
}

async fn folders(store: &dyn Store) {
    let archive = archive(store).await;
    let account = import_account(store, archive).await;
    let inbox = store
        .upsert_folder(account.id, "INBOX", None)
        .await
        .unwrap();
    let again = store
        .upsert_folder(account.id, "INBOX", Some(FolderRole::Inbox))
        .await
        .unwrap();
    assert_eq!(again.id, inbox.id, "upsert_folder created a second folder");
    assert_eq!(again.role, Some(FolderRole::Inbox), "role was not updated");
    store
        .upsert_folder(account.id, "Archive/2024", Some(FolderRole::Archive))
        .await
        .unwrap();

    let paths: Vec<_> = store
        .list_folders(archive)
        .await
        .unwrap()
        .into_iter()
        .map(|f| (f.folder.path, f.messages, f.unread))
        .collect();
    assert_eq!(
        paths,
        [("Archive/2024".into(), 0, 0), ("INBOX".to_string(), 0, 0)]
    );
    assert_eq!(
        store.get_folder(inbox.id).await.unwrap().unwrap().path,
        "INBOX"
    );
}

async fn insert_is_idempotent(store: &dyn Store) {
    let archive = archive(store).await;
    let mut new = message(archive, 1, 1_700_000_000);
    new.attachments = vec![
        Attachment {
            ordinal: 0,
            blob_id: BlobId::of(b"logo"),
            filename: Some("logo.png".into()),
            content_type: "image/png".into(),
            size: 4,
            content_id: Some("logo@example.com".into()),
            inline: true,
        },
        Attachment {
            ordinal: 1,
            blob_id: BlobId::of(b"report"),
            filename: Some("report.pdf".into()),
            content_type: "application/pdf".into(),
            size: 6,
            content_id: None,
            inline: false,
        },
    ];
    let first = store.insert_message(new.clone()).await.unwrap();
    assert!(first.created);
    let second = store.insert_message(new.clone()).await.unwrap();
    assert_eq!(
        second.message_id, first.message_id,
        "same bytes made a second message"
    );
    assert!(!second.created);

    let stored = store.get_message(first.message_id).await.unwrap().unwrap();
    assert_eq!(stored.subject.as_deref(), Some("Subject 1"));
    assert_eq!(stored.from, new.from);
    assert_eq!(stored.to, new.to);
    assert_eq!(stored.date, Timestamp::from_secs(1_700_000_000));
    assert_eq!(stored.attachment_count, 1, "inline parts were counted");
    assert_eq!(
        store.attachments(first.message_id).await.unwrap(),
        new.attachments
    );
    assert_eq!(
        store.find_by_blob(archive, &new.blob_id).await.unwrap(),
        Some(first.message_id)
    );

    let other = store.create_archive("Other").await.unwrap().id;
    let mut copy = new.clone();
    copy.archive_id = other;
    let elsewhere = store.insert_message(copy).await.unwrap();
    assert!(
        elsewhere.created,
        "the same bytes in another archive must be a new message"
    );

    let stats = store.archive_stats(archive).await.unwrap();
    assert_eq!(stats.messages, 1);
    assert_eq!(stats.attachments, 2);
    assert_eq!(stats.bytes, new.size);
    assert_eq!(stats.oldest, Some(Timestamp::from_secs(1_700_000_000)));

    let mut undated = message(archive, 2, 0);
    undated.sent_at = None;
    undated.received_at = Some(Timestamp::from_secs(1_600_000_000));
    let undated = store.insert_message(undated).await.unwrap();
    let undated = store
        .get_message(undated.message_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        undated.date,
        Timestamp::from_secs(1_600_000_000),
        "date did not fall back"
    );
}

async fn threads_merge_in_any_order(store: &dyn Store) {
    let archive = archive(store).await;
    let root = message(archive, 10, 1_700_000_000);
    let mut reply = message(archive, 11, 1_700_000_100);
    reply.parent_ids = vec!["m10@example.com".into()];
    let mut second_reply = message(archive, 12, 1_700_000_200);
    second_reply.parent_ids = vec!["m10@example.com".into(), "m11@example.com".into()];
    let unrelated = message(archive, 13, 1_700_000_300);

    let second_reply = store.insert_message(second_reply).await.unwrap();
    let reply = store.insert_message(reply).await.unwrap();
    let unrelated = store.insert_message(unrelated).await.unwrap();
    let root = store.insert_message(root).await.unwrap();

    let thread = store
        .get_message(root.message_id)
        .await
        .unwrap()
        .unwrap()
        .thread_id;
    for id in [reply.message_id, second_reply.message_id] {
        assert_eq!(
            store.get_message(id).await.unwrap().unwrap().thread_id,
            thread,
            "replies are not in the root's thread"
        );
    }
    assert_ne!(
        store
            .get_message(unrelated.message_id)
            .await
            .unwrap()
            .unwrap()
            .thread_id,
        thread
    );
    let order: Vec<_> = store
        .thread_messages(thread)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(
        order,
        [root.message_id, reply.message_id, second_reply.message_id]
    );

    let mut stray = message(store.create_archive("Elsewhere").await.unwrap().id, 14, 0);
    stray.parent_ids = vec!["m10@example.com".into()];
    let stray = store.insert_message(stray).await.unwrap();
    assert_ne!(stray.thread_id, thread, "threads crossed archives");
}

async fn lists_page_newest_first(store: &dyn Store) {
    let archive = archive(store).await;
    let account = import_account(store, archive).await;
    let folder = store
        .upsert_folder(account.id, "INBOX", None)
        .await
        .unwrap();
    let mut ids = Vec::new();
    for n in 0..7 {
        let inserted = store
            .insert_message(message(archive, 100 + n, 1_700_000_000 + i64::from(n % 3)))
            .await
            .unwrap();
        if n % 2 == 0 {
            store
                .record_location(
                    inserted.message_id,
                    folder.id,
                    Some(u64::from(n) + 1),
                    &[],
                    Timestamp::now(),
                )
                .await
                .unwrap();
        }
        ids.push(inserted.message_id);
    }

    let mut seen = Vec::new();
    let mut after = None;
    loop {
        let page = store
            .list_messages(&MessageQuery {
                archive_id: archive,
                folder_id: None,
                label: None,
                after: after.clone(),
                limit: 3,
            })
            .await
            .unwrap();
        assert!(page.items.len() <= 3);
        seen.extend(page.items.iter().map(|m| (m.date, m.id)));
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    assert_eq!(seen.len(), 7, "pages lost or repeated messages");
    let mut sorted = seen.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(seen, sorted, "list is not newest first with id tie-break");

    let in_folder = store
        .list_messages(&MessageQuery {
            archive_id: archive,
            folder_id: Some(folder.id),
            label: None,
            after: None,
            limit: 100,
        })
        .await
        .unwrap();
    assert_eq!(in_folder.items.len(), 4);
    assert!(in_folder.next.is_none());

    let picked = store
        .get_messages(&[ids[3], ids[0], MessageId::new()])
        .await
        .unwrap();
    assert_eq!(
        picked.iter().map(|m| m.id).collect::<Vec<_>>(),
        [ids[3], ids[0]],
        "get_messages did not keep the requested order"
    );
}

async fn locations_follow_the_server(store: &dyn Store) {
    let archive = archive(store).await;
    let account = import_account(store, archive).await;
    let inbox = store
        .upsert_folder(account.id, "INBOX", None)
        .await
        .unwrap();
    let a = store
        .insert_message(message(archive, 200, 1))
        .await
        .unwrap()
        .message_id;
    let b = store
        .insert_message(message(archive, 201, 2))
        .await
        .unwrap()
        .message_id;
    let seen = ["\\Seen".to_string()];

    store
        .record_location(a, inbox.id, Some(5), &[], Timestamp::from_secs(10))
        .await
        .unwrap();
    store
        .record_location(a, inbox.id, Some(5), &seen, Timestamp::from_secs(20))
        .await
        .unwrap();
    let [location] = store.locations(a).await.unwrap().try_into().unwrap();
    assert_eq!(location.flags, seen);
    assert_eq!(location.first_seen_at, Timestamp::from_secs(10));
    assert_eq!(location.last_seen_at, Timestamp::from_secs(20));
    assert_eq!(store.message_at_uid(inbox.id, 5).await.unwrap(), Some(a));

    store
        .record_location(b, inbox.id, Some(6), &[], Timestamp::from_secs(20))
        .await
        .unwrap();
    assert_eq!(store.folder_uids(inbox.id).await.unwrap(), [5, 6]);
    let counts = store.list_folders(archive).await.unwrap();
    assert_eq!(counts[0].messages, 2);

    store
        .mark_removed(inbox.id, &[5], Timestamp::from_secs(30))
        .await
        .unwrap();
    assert_eq!(store.folder_uids(inbox.id).await.unwrap(), [6]);
    assert_eq!(
        store.locations(a).await.unwrap()[0].removed_at,
        Some(Timestamp::from_secs(30))
    );
    assert!(
        store.get_message(a).await.unwrap().is_some(),
        "removal deleted the message"
    );

    store
        .record_location(a, inbox.id, Some(5), &[], Timestamp::from_secs(40))
        .await
        .unwrap();
    assert_eq!(store.locations(a).await.unwrap()[0].removed_at, None);

    store.forget_uids(inbox.id).await.unwrap();
    assert!(store.folder_uids(inbox.id).await.unwrap().is_empty());
    assert_eq!(
        store.locations(a).await.unwrap().len(),
        1,
        "forget_uids dropped locations"
    );
    store
        .record_location(b, inbox.id, Some(1), &[], Timestamp::from_secs(50))
        .await
        .unwrap();
    store
        .record_location(a, inbox.id, Some(1), &[], Timestamp::from_secs(60))
        .await
        .unwrap();
    assert_eq!(
        store.message_at_uid(inbox.id, 1).await.unwrap(),
        Some(a),
        "a UID did not move to its new message"
    );
    assert_eq!(store.locations(b).await.unwrap()[0].uid, None);
}

async fn extras_and_unread(store: &dyn Store) {
    let archive = archive(store).await;
    let account = import_account(store, archive).await;
    let inbox = store
        .upsert_folder(account.id, "INBOX", Some(FolderRole::Inbox))
        .await
        .unwrap();
    let starred = store
        .upsert_folder(account.id, "Starred", Some(FolderRole::Flagged))
        .await
        .unwrap();
    let parent = store
        .insert_message(message(archive, 300, 1))
        .await
        .unwrap()
        .message_id;
    let mut reply = message(archive, 301, 2);
    reply.parent_ids = vec!["m300@example.com".into()];
    let reply = store.insert_message(reply).await.unwrap().message_id;
    let alone = store
        .insert_message(message(archive, 302, 3))
        .await
        .unwrap()
        .message_id;
    let flags = |list: &[&str]| list.iter().map(|f| f.to_string()).collect::<Vec<_>>();
    let at = Timestamp::from_secs(10);

    store
        .record_location(parent, inbox.id, Some(1), &flags(&["\\seen"]), at)
        .await
        .unwrap();
    store
        .record_location(
            parent,
            starred.id,
            Some(1),
            &flags(&["\\Seen", "\\Flagged"]),
            at,
        )
        .await
        .unwrap();
    store
        .record_location(reply, inbox.id, Some(2), &[], at)
        .await
        .unwrap();
    store
        .record_location(alone, inbox.id, Some(3), &flags(&["$label1"]), at)
        .await
        .unwrap();
    store
        .record_location(alone, starred.id, Some(3), &flags(&["\\Flagged"]), at)
        .await
        .unwrap();
    store
        .mark_removed(starred.id, &[3], Timestamp::from_secs(20))
        .await
        .unwrap();

    let counts: Vec<_> = store
        .list_folders(archive)
        .await
        .unwrap()
        .into_iter()
        .map(|f| (f.folder.path, f.messages, f.unread))
        .collect();
    assert_eq!(
        counts,
        [("INBOX".into(), 3, 2), ("Starred".to_string(), 1, 0)],
        "unread counts are wrong"
    );

    let missing = MessageId::new();
    let all = store
        .message_extras(&[parent, reply, alone, missing], None)
        .await
        .unwrap();
    assert_eq!(all.len(), 3, "extras for a missing message");
    let mut both = vec![inbox.id, starred.id];
    both.sort();
    assert_eq!(
        all[&parent],
        MessageExtras {
            flags: flags(&["\\Flagged", "\\Seen", "\\seen"]),
            thread_count: 2,
            folder_ids: both,
            labels: Vec::new(),
        }
    );
    assert_eq!(all[&reply].thread_count, 2);
    assert!(all[&reply].flags.is_empty());
    assert_eq!(
        all[&alone],
        MessageExtras {
            flags: flags(&["$label1"]),
            thread_count: 1,
            folder_ids: vec![inbox.id],
            labels: Vec::new(),
        },
        "a removed location counted"
    );

    let in_inbox = store
        .message_extras(&[parent, alone], Some(inbox.id))
        .await
        .unwrap();
    assert_eq!(in_inbox[&parent].flags, flags(&["\\seen"]));
    assert_eq!(in_inbox[&parent].folder_ids.len(), 2);
    let in_starred = store
        .message_extras(&[alone], Some(starred.id))
        .await
        .unwrap();
    assert!(in_starred[&alone].flags.is_empty());
    assert!(store.message_extras(&[], None).await.unwrap().is_empty());
}

async fn flag_changes(store: &dyn Store) {
    let archive = archive(store).await;
    let account = import_account(store, archive).await;
    let inbox = store
        .upsert_folder(account.id, "INBOX", Some(FolderRole::Inbox))
        .await
        .unwrap();
    let message = store
        .insert_message(message(archive, 400, 1))
        .await
        .unwrap()
        .message_id;
    let strings = |list: &[&str]| list.iter().map(|f| f.to_string()).collect::<Vec<_>>();
    store
        .record_location(
            message,
            inbox.id,
            Some(4),
            &strings(&["\\Flagged"]),
            Timestamp::from_secs(1),
        )
        .await
        .unwrap();

    assert!(
        store
            .set_flags(inbox.id, 4, &strings(&["\\Seen", "\\Flagged"]))
            .await
            .unwrap()
    );
    let extras = store.message_extras(&[message], None).await.unwrap();
    assert_eq!(extras[&message].flags, strings(&["\\Flagged", "\\Seen"]));
    assert_eq!(store.list_folders(archive).await.unwrap()[0].unread, 0);
    assert!(
        !store
            .set_flags(inbox.id, 4, &strings(&["\\Flagged", "\\Seen", "\\Seen"]))
            .await
            .unwrap(),
        "the same flags in another order counted as a change"
    );
    assert!(
        !store.set_flags(inbox.id, 99, &[]).await.unwrap(),
        "an unknown UID counted as a change"
    );
    store
        .mark_removed(inbox.id, &[4], Timestamp::from_secs(2))
        .await
        .unwrap();
    assert!(store.set_flags(inbox.id, 4, &[]).await.unwrap());
    assert!(store.locations(message).await.unwrap()[0].flags.is_empty());
}

async fn labels(store: &dyn Store) {
    let archive = archive(store).await;
    let other = store.create_archive("Other").await.unwrap().id;
    let account = import_account(store, archive).await;
    let all = store
        .upsert_folder(account.id, "[Gmail]/All Mail", Some(FolderRole::All))
        .await
        .unwrap();
    let mut ids = Vec::new();
    for n in 0..3 {
        let id = store
            .insert_message(message(archive, 500 + n, i64::from(n)))
            .await
            .unwrap()
            .message_id;
        ids.push(id);
    }
    let elsewhere = store
        .insert_message(message(other, 500, 0))
        .await
        .unwrap()
        .message_id;
    let strings = |list: &[&str]| list.iter().map(|f| f.to_string()).collect::<Vec<_>>();
    store
        .record_location(
            ids[0],
            all.id,
            Some(1),
            &strings(&["\\Seen"]),
            Timestamp::from_secs(1),
        )
        .await
        .unwrap();
    store
        .record_location(ids[1], all.id, Some(2), &[], Timestamp::from_secs(1))
        .await
        .unwrap();

    assert!(
        store
            .set_labels(ids[0], &strings(&["Inbox", "Projects/2025", "Inbox", ""]))
            .await
            .unwrap()
    );
    assert!(
        !store
            .set_labels(ids[0], &strings(&["Projects/2025", "Inbox"]))
            .await
            .unwrap(),
        "the same labels counted as a change"
    );
    store
        .set_labels(ids[1], &strings(&["Inbox"]))
        .await
        .unwrap();
    store.set_labels(ids[2], &strings(&["Old"])).await.unwrap();
    store
        .set_labels(elsewhere, &strings(&["Inbox"]))
        .await
        .unwrap();
    assert_eq!(
        store.labels(ids[0]).await.unwrap(),
        strings(&["Inbox", "Projects/2025"])
    );
    assert!(store.labels(MessageId::new()).await.unwrap().is_empty());
    assert!(matches!(
        store.set_labels(MessageId::new(), &[]).await,
        Err(StoreError::NotFound(_))
    ));

    assert!(store.set_labels(ids[2], &[]).await.unwrap());
    assert_eq!(
        store.list_labels(archive).await.unwrap(),
        [
            LabelSummary {
                name: "Inbox".into(),
                messages: 2,
                unread: 1,
            },
            LabelSummary {
                name: "Projects/2025".into(),
                messages: 1,
                unread: 0,
            },
        ],
        "label counts are wrong, or a label without messages is listed"
    );

    let extras = store.message_extras(&[ids[0], ids[2]], None).await.unwrap();
    assert_eq!(extras[&ids[0]].labels, strings(&["Inbox", "Projects/2025"]));
    assert!(extras[&ids[2]].labels.is_empty());

    let labeled = store
        .list_messages(&MessageQuery {
            archive_id: archive,
            folder_id: None,
            label: Some("Inbox".into()),
            after: None,
            limit: 1,
        })
        .await
        .unwrap();
    assert_eq!(labeled.items[0].id, ids[1], "not newest first");
    let rest = store
        .list_messages(&MessageQuery {
            archive_id: archive,
            folder_id: None,
            label: Some("Inbox".into()),
            after: labeled.next,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(
        rest.items.iter().map(|m| m.id).collect::<Vec<_>>(),
        [ids[0]],
        "the label filter leaked another archive or lost a page"
    );
    let both = store
        .list_messages(&MessageQuery {
            archive_id: archive,
            folder_id: Some(all.id),
            label: Some("Projects/2025".into()),
            after: None,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(both.items.len(), 1);
    let none = store
        .list_messages(&MessageQuery {
            archive_id: archive,
            folder_id: None,
            label: Some("inbox".into()),
            after: None,
            limit: 10,
        })
        .await
        .unwrap();
    assert!(none.items.is_empty(), "labels compared ignoring case");
}

async fn sync_state(store: &dyn Store) {
    let archive = archive(store).await;
    let account = import_account(store, archive).await;
    let folder = store
        .upsert_folder(account.id, "INBOX", None)
        .await
        .unwrap();
    assert_eq!(
        store.cursor(folder.id).await.unwrap(),
        FolderCursor::default()
    );
    let cursor = FolderCursor {
        uid_validity: Some(42),
        last_uid: Some(100),
        highest_modseq: Some(7),
        flags_checked_at: Some(Timestamp::from_secs(2)),
        state: Some("opaque".into()),
        synced_at: Some(Timestamp::from_secs(1)),
    };
    store.set_cursor(folder.id, &cursor).await.unwrap();
    assert_eq!(store.cursor(folder.id).await.unwrap(), cursor);

    store
        .record_failure(folder.id, 9, "parse error", Timestamp::from_secs(1))
        .await
        .unwrap();
    store
        .record_failure(folder.id, 9, "timeout", Timestamp::from_secs(2))
        .await
        .unwrap();
    let [failure] = store.failures(folder.id).await.unwrap().try_into().unwrap();
    assert_eq!(failure.attempts, 2);
    assert_eq!(failure.reason, "timeout");
    assert_eq!(failure.failed_at, Timestamp::from_secs(2));
    store.clear_failure(folder.id, 9).await.unwrap();
    assert!(store.failures(folder.id).await.unwrap().is_empty());
}

fn new_user(email: &str, role: InstanceRole) -> NewUser {
    NewUser {
        email: email.into(),
        name: "Ada Lovelace".into(),
        role,
        password_hash: Some("$argon2id$v=19$m=8,t=1,p=1$c2FsdHNhbHQ$aGFzaA".into()),
    }
}

async fn users(store: &dyn Store) {
    assert_eq!(
        store.count_users().await.unwrap(),
        0,
        "the store is not empty"
    );
    let ada = store
        .create_user(new_user("Ada@Example.com", InstanceRole::Owner))
        .await
        .unwrap();
    assert_eq!(
        ada.email, "Ada@Example.com",
        "email was not kept as entered"
    );
    assert_eq!(ada.role, InstanceRole::Owner);
    assert_eq!(store.count_users().await.unwrap(), 1);
    assert_eq!(store.get_user(ada.id).await.unwrap(), Some(ada.clone()));
    assert_eq!(
        store.find_user_by_email("ada@EXAMPLE.com").await.unwrap(),
        Some(ada.clone()),
        "email lookup is case-sensitive"
    );
    assert_eq!(
        store.find_user_by_email("bob@example.com").await.unwrap(),
        None
    );
    assert!(store.get_user(UserId::new()).await.unwrap().is_none());

    let duplicate = store
        .create_user(new_user("ADA@example.COM", InstanceRole::Member))
        .await;
    assert!(
        matches!(duplicate, Err(StoreError::Invalid(_))),
        "a second user with the same email"
    );
    assert_eq!(store.count_users().await.unwrap(), 1);

    let mut no_password = new_user("bob@example.org", InstanceRole::Member);
    no_password.password_hash = None;
    let bob = store.create_user(no_password).await.unwrap();
    assert_eq!(store.password_hash(bob.id).await.unwrap(), None);
    assert_eq!(
        store.password_hash(ada.id).await.unwrap().as_deref(),
        Some("$argon2id$v=19$m=8,t=1,p=1$c2FsdHNhbHQ$aGFzaA")
    );
    store.set_password_hash(bob.id, "$new").await.unwrap();
    assert_eq!(
        store.password_hash(bob.id).await.unwrap().as_deref(),
        Some("$new")
    );
    assert!(matches!(
        store.password_hash(UserId::new()).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        store.set_password_hash(UserId::new(), "$x").await,
        Err(StoreError::NotFound(_))
    ));
    let ids: Vec<_> = store
        .list_users()
        .await
        .unwrap()
        .into_iter()
        .map(|u| u.id)
        .collect();
    assert_eq!(ids, [ada.id, bob.id], "users are not oldest first");
}

async fn sessions(store: &dyn Store) {
    let user = store
        .create_user(new_user("carol@example.com", InstanceRole::Admin))
        .await
        .unwrap();
    let other = store
        .create_user(new_user("dave@example.com", InstanceRole::Member))
        .await
        .unwrap();
    let live = [1u8; 32];
    let short = [2u8; 32];
    let others = [3u8; 32];
    store
        .create_session(&live, user.id, Timestamp::from_secs(1_000))
        .await
        .unwrap();
    store
        .create_session(&short, user.id, Timestamp::from_secs(100))
        .await
        .unwrap();
    store
        .create_session(&others, other.id, Timestamp::from_secs(1_000))
        .await
        .unwrap();
    assert!(
        matches!(
            store
                .create_session(&live, other.id, Timestamp::from_secs(5_000))
                .await,
            Err(StoreError::Invalid(_))
        ),
        "a session hash was reused"
    );
    assert!(matches!(
        store
            .create_session(&[4u8; 32], UserId::new(), Timestamp::from_secs(1_000))
            .await,
        Err(StoreError::NotFound(_))
    ));

    let at = |secs| Timestamp::from_secs(secs);
    assert_eq!(
        store.session_user(&live, at(50)).await.unwrap(),
        Some(user.clone())
    );
    assert_eq!(
        store.session_user(&short, at(99)).await.unwrap(),
        Some(user.clone())
    );
    assert_eq!(
        store.session_user(&short, at(100)).await.unwrap(),
        None,
        "an expired session still signs in"
    );
    assert_eq!(store.session_user(&[9u8; 32], at(0)).await.unwrap(), None);

    assert_eq!(store.delete_expired_sessions(at(500)).await.unwrap(), 1);
    assert_eq!(store.delete_expired_sessions(at(500)).await.unwrap(), 0);
    assert_eq!(store.session_user(&short, at(0)).await.unwrap(), None);

    store.delete_session(&live).await.unwrap();
    store.delete_session(&live).await.unwrap();
    assert_eq!(store.session_user(&live, at(50)).await.unwrap(), None);

    store
        .create_session(&live, user.id, at(1_000))
        .await
        .unwrap();
    store.delete_user_sessions(user.id).await.unwrap();
    assert_eq!(store.session_user(&live, at(50)).await.unwrap(), None);
    assert_eq!(
        store.session_user(&others, at(50)).await.unwrap(),
        Some(other),
        "delete_user_sessions ended another user's session"
    );
}

async fn removal(store: &dyn Store) {
    let gone = store.create_archive("Old").await.unwrap().id;
    let kept = store.create_archive("Kept").await.unwrap().id;
    store.rename_archive(kept, "Kept mail").await.unwrap();
    assert_eq!(
        store.get_archive(kept).await.unwrap().unwrap().name,
        "Kept mail"
    );
    assert!(matches!(
        store.rename_archive(ArchiveId::new(), "x").await,
        Err(StoreError::NotFound(_))
    ));

    let attachment = BlobId::of(b"attachment only in the old archive");
    let mut shared = message(gone, 9001, 1_700_000_000);
    shared.attachments.push(Attachment {
        ordinal: 0,
        blob_id: attachment,
        filename: Some("a.txt".into()),
        content_type: "text/plain".into(),
        size: 3,
        content_id: None,
        inline: false,
    });
    let own = message(gone, 9002, 1_700_000_001);
    let own_blob = own.blob_id;
    let in_gone = store.insert_message(shared).await.unwrap().message_id;
    store.insert_message(own).await.unwrap();
    let in_kept = store
        .insert_message(message(kept, 9001, 1_700_000_000))
        .await
        .unwrap()
        .message_id;

    let account = import_account(store, gone).await;
    let folder = store
        .upsert_folder(account.id, "INBOX", Some(FolderRole::Inbox))
        .await
        .unwrap()
        .id;
    store
        .record_location(in_gone, folder, Some(1), &[], Timestamp::from_secs(1))
        .await
        .unwrap();
    store
        .set_cursor(
            folder,
            &FolderCursor {
                last_uid: Some(1),
                ..FolderCursor::default()
            },
        )
        .await
        .unwrap();
    store
        .record_failure(folder, 2, "broken", Timestamp::from_secs(1))
        .await
        .unwrap();

    store.set_labels(in_gone, &["Work".into()]).await.unwrap();
    store.set_labels(in_kept, &["Work".into()]).await.unwrap();

    let mut orphans = vec![attachment, own_blob];
    orphans.sort();
    assert_eq!(
        store.delete_archive(gone).await.unwrap(),
        orphans,
        "orphaned blobs are wrong; a blob still used by another archive must not be listed"
    );
    assert!(store.get_archive(gone).await.unwrap().is_none());
    assert!(store.get_account(account.id).await.unwrap().is_none());
    assert!(store.get_message(in_gone).await.unwrap().is_none());
    assert!(store.get_message(in_kept).await.unwrap().is_some());
    assert!(store.list_labels(gone).await.unwrap().is_empty());
    assert_eq!(store.labels(in_kept).await.unwrap(), ["Work"]);
    assert!(matches!(
        store.delete_archive(gone).await,
        Err(StoreError::NotFound(_))
    ));

    let account = import_account(store, kept).await;
    let folder = store
        .upsert_folder(account.id, "Sent", None)
        .await
        .unwrap()
        .id;
    store
        .record_location(in_kept, folder, Some(5), &[], Timestamp::from_secs(1))
        .await
        .unwrap();
    store
        .record_failure(folder, 6, "broken", Timestamp::from_secs(1))
        .await
        .unwrap();
    store.delete_account(account.id).await.unwrap();
    assert!(store.get_account(account.id).await.unwrap().is_none());
    assert!(store.list_folders(kept).await.unwrap().is_empty());
    assert!(
        store.get_message(in_kept).await.unwrap().is_some(),
        "removing an account removed its messages"
    );
    assert!(store.locations(in_kept).await.unwrap().is_empty());
    assert!(matches!(
        store.delete_account(account.id).await,
        Err(StoreError::NotFound(_))
    ));
    store.delete_archive(kept).await.unwrap();
}

async fn user_changes_and_grants(store: &dyn Store) {
    let grace = store
        .create_user(new_user("grace@example.com", InstanceRole::Member))
        .await
        .unwrap();
    let alan = store
        .create_user(new_user("alan@example.org", InstanceRole::Member))
        .await
        .unwrap();
    let changed = User {
        email: "Grace@Example.org".into(),
        name: "Grace Hopper".into(),
        role: InstanceRole::Admin,
        ..grace.clone()
    };
    store.update_user(&changed).await.unwrap();
    assert_eq!(store.get_user(grace.id).await.unwrap().unwrap(), changed);
    assert_eq!(
        store
            .find_user_by_email("grace@example.org")
            .await
            .unwrap()
            .map(|u| u.id),
        Some(grace.id)
    );
    assert!(matches!(
        store
            .update_user(&User {
                email: "ALAN@example.org".into(),
                ..changed.clone()
            })
            .await,
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        store
            .update_user(&User {
                id: UserId::new(),
                ..changed.clone()
            })
            .await,
        Err(StoreError::NotFound(_))
    ));

    let first = store.create_archive("First").await.unwrap().id;
    let second = store.create_archive("Second").await.unwrap().id;
    assert!(store.grants(alan.id).await.unwrap().is_empty());
    store
        .set_grants(alan.id, &[second, ArchiveId::new(), first, first])
        .await
        .unwrap();
    assert_eq!(store.grants(alan.id).await.unwrap(), [first, second]);
    store.set_grants(alan.id, &[second]).await.unwrap();
    assert_eq!(store.grants(alan.id).await.unwrap(), [second]);
    assert!(matches!(
        store.set_grants(UserId::new(), &[first]).await,
        Err(StoreError::NotFound(_))
    ));
    store.delete_archive(second).await.unwrap();
    assert!(
        store.grants(alan.id).await.unwrap().is_empty(),
        "a removed archive stayed granted"
    );

    store.set_grants(alan.id, &[first]).await.unwrap();
    let hash: SessionHash = [42; 32];
    store
        .create_session(&hash, alan.id, Timestamp::from_secs(4_000_000_000))
        .await
        .unwrap();
    store.delete_user(alan.id).await.unwrap();
    assert!(store.get_user(alan.id).await.unwrap().is_none());
    assert!(
        store
            .session_user(&hash, Timestamp::from_secs(1))
            .await
            .unwrap()
            .is_none(),
        "a removed user kept a session"
    );
    assert!(store.grants(alan.id).await.unwrap().is_empty());
    assert!(matches!(
        store.delete_user(alan.id).await,
        Err(StoreError::NotFound(_))
    ));
    store.delete_archive(first).await.unwrap();
}

async fn remote_copies(store: &dyn Store) {
    let first = store.create_archive("Newsletters").await.unwrap();
    assert!(
        !first.save_remote_images,
        "saving remote images is on by default"
    );
    assert_eq!(first.save_remote_images_since, None);
    assert!(matches!(
        store.set_save_remote_images(ArchiveId::new(), true).await,
        Err(StoreError::NotFound(_))
    ));
    let second = store.create_archive("Elsewhere").await.unwrap().id;

    let a = store
        .insert_message(message(first.id, 9101, 1_700_000_000))
        .await
        .unwrap()
        .message_id;
    let b = store
        .insert_message(message(first.id, 9102, 1_700_000_001))
        .await
        .unwrap()
        .message_id;
    let c = store
        .insert_message(message(second, 9103, 1_700_000_002))
        .await
        .unwrap()
        .message_id;
    let all = ArchivedWhen::Any;
    assert_eq!(
        store.unscanned_messages(first.id, all, 10).await.unwrap(),
        [a, b]
    );
    assert_eq!(store.count_unscanned(first.id, all).await.unwrap(), 2);

    std::thread::sleep(std::time::Duration::from_millis(5));
    store.set_save_remote_images(first.id, true).await.unwrap();
    let since = store
        .get_archive(first.id)
        .await
        .unwrap()
        .unwrap()
        .save_remote_images_since
        .expect("turning saving on records when");
    store.set_save_remote_images(first.id, true).await.unwrap();
    assert_eq!(
        store
            .get_archive(first.id)
            .await
            .unwrap()
            .unwrap()
            .save_remote_images_since,
        Some(since),
        "turning saving on again moved its start"
    );
    let later = store
        .insert_message(message(first.id, 9104, 1_600_000_000))
        .await
        .unwrap()
        .message_id;
    assert_eq!(
        store
            .unscanned_messages(first.id, ArchivedWhen::Since(since), 10)
            .await
            .unwrap(),
        [later],
        "messages are picked by when they were archived, not sent"
    );
    assert_eq!(
        store
            .count_unscanned(first.id, ArchivedWhen::Before(since))
            .await
            .unwrap(),
        2
    );
    store.record_remote_scan(later, &[]).await.unwrap();

    let logo_blob = BlobId::of(b"logo bytes");
    let logo = RemoteCopy {
        key: "logo".into(),
        url: "https://example.com/logo.png".into(),
        blob_id: Some(logo_blob),
        content_type: Some("image/png".into()),
        size: 10,
        fetched_at: Timestamp::from_secs(1),
        error: None,
    };
    let broken = RemoteCopy {
        key: "broken".into(),
        url: "https://example.org/gone.png".into(),
        blob_id: None,
        content_type: None,
        size: 0,
        fetched_at: Timestamp::from_secs(1),
        error: Some("HTTP 404".into()),
    };
    store.save_remote_copy(&logo).await.unwrap();
    store.save_remote_copy(&broken).await.unwrap();
    store.save_remote_copy(&broken).await.unwrap();
    assert_eq!(store.remote_copy("logo").await.unwrap(), Some(logo.clone()));
    assert_eq!(store.remote_copy("missing").await.unwrap(), None);

    let keys = ["logo".to_string(), "broken".into(), "unknown".into()];
    store.record_remote_scan(a, &keys).await.unwrap();
    store.record_remote_scan(a, &keys).await.unwrap();
    store.record_remote_scan(c, &keys[..1]).await.unwrap();
    assert!(matches!(
        store.record_remote_scan(MessageId::new(), &[]).await,
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(
        store.unscanned_messages(first.id, all, 10).await.unwrap(),
        [b]
    );
    assert_eq!(
        store.remote_copy_totals(first.id).await.unwrap(),
        RemoteCopyTotals {
            images: 1,
            bytes: 10,
            failed: 1,
        },
        "copies counted twice or in the wrong column"
    );
    assert_eq!(
        store
            .failed_remote_copies(first.id, Timestamp::from_secs(2), 10)
            .await
            .unwrap(),
        std::slice::from_ref(&broken)
    );
    assert!(
        store
            .failed_remote_copies(first.id, Timestamp::from_secs(1), 10)
            .await
            .unwrap()
            .is_empty(),
        "a copy tried after the cut-off was listed"
    );
    assert_eq!(
        store.message_remote_copies(a).await.unwrap(),
        std::slice::from_ref(&logo),
        "only copies with saved bytes are listed"
    );
    assert!(store.message_remote_copies(b).await.unwrap().is_empty());

    store.set_save_remote_images(first.id, false).await.unwrap();
    let off = store.get_archive(first.id).await.unwrap().unwrap();
    assert!(!off.save_remote_images);
    assert_eq!(off.save_remote_images_since, None);

    let orphans = store.delete_archive(first.id).await.unwrap();
    assert!(
        !orphans.contains(&logo_blob),
        "a copy another archive still links was orphaned"
    );
    assert_eq!(store.remote_copy("broken").await.unwrap(), None);
    assert_eq!(store.remote_copy("logo").await.unwrap(), Some(logo));

    let orphans = store.delete_archive(second).await.unwrap();
    assert!(
        orphans.contains(&logo_blob),
        "the last link did not free the copy"
    );
    assert_eq!(store.remote_copy("logo").await.unwrap(), None);
}

async fn derived(store: &dyn Store) {
    let source = BlobId::of(b"derived: a picture");
    let thumb = BlobId::of(b"derived: its thumbnail");
    let text = BlobId::of(b"derived: its text");
    assert_eq!(
        store
            .derived(&source, DerivedKind::Thumbnail)
            .await
            .unwrap(),
        None
    );
    store
        .set_derived(&source, DerivedKind::Thumbnail, &Derived::Nothing)
        .await
        .unwrap();
    store
        .set_derived(&source, DerivedKind::Thumbnail, &Derived::Blob(thumb))
        .await
        .unwrap();
    store
        .set_derived(&source, DerivedKind::Thumbnail, &Derived::Blob(thumb))
        .await
        .unwrap();
    store
        .set_derived(&source, DerivedKind::Text, &Derived::Blob(text))
        .await
        .unwrap();
    assert_eq!(
        store
            .derived(&source, DerivedKind::Thumbnail)
            .await
            .unwrap(),
        Some(Derived::Blob(thumb))
    );
    assert_eq!(
        store.derived(&source, DerivedKind::Text).await.unwrap(),
        Some(Derived::Blob(text))
    );

    let other = BlobId::of(b"derived: the same text elsewhere");
    store
        .set_derived(&other, DerivedKind::Text, &Derived::Blob(text))
        .await
        .unwrap();
    let none = BlobId::of(b"derived: no text");
    store
        .set_derived(&none, DerivedKind::Text, &Derived::Nothing)
        .await
        .unwrap();
    assert_eq!(
        store.clear_derived(DerivedKind::Text).await.unwrap(),
        vec![text],
        "a cleared text blob is listed once, however many sources shared it"
    );
    assert_eq!(
        store.derived(&source, DerivedKind::Text).await.unwrap(),
        None
    );
    assert_eq!(store.derived(&none, DerivedKind::Text).await.unwrap(), None);
    assert_eq!(
        store
            .derived(&source, DerivedKind::Thumbnail)
            .await
            .unwrap(),
        Some(Derived::Blob(thumb)),
        "clearing text touched thumbnails"
    );

    let shared = BlobId::of(b"derived: text two sources share");
    let first = BlobId::of(b"derived: first source");
    let second = BlobId::of(b"derived: second source");
    for source in [&first, &second] {
        store
            .set_derived(source, DerivedKind::Text, &Derived::Blob(shared))
            .await
            .unwrap();
    }
    assert_eq!(
        store
            .forget_derived(&first, DerivedKind::Text)
            .await
            .unwrap(),
        None,
        "a text still used by another source was returned"
    );
    assert_eq!(
        store.derived(&first, DerivedKind::Text).await.unwrap(),
        None
    );
    assert_eq!(
        store
            .forget_derived(&second, DerivedKind::Text)
            .await
            .unwrap(),
        Some(shared)
    );
    assert_eq!(
        store
            .forget_derived(&second, DerivedKind::Text)
            .await
            .unwrap(),
        None,
        "forgetting twice"
    );

    let archive = store.create_archive("Derived").await.unwrap().id;
    let mut with_picture = message(archive, 9301, 1_700_000_000);
    with_picture.attachments.push(Attachment {
        ordinal: 0,
        blob_id: source,
        filename: Some("picture.png".into()),
        content_type: "image/png".into(),
        size: 18,
        content_id: None,
        inline: false,
    });
    let message_blob = with_picture.blob_id;
    store.insert_message(with_picture).await.unwrap();
    let mut orphans = vec![source, thumb, message_blob];
    orphans.sort();
    assert_eq!(
        store.delete_archive(archive).await.unwrap(),
        orphans,
        "deleting an archive must also release what was derived from its blobs"
    );
    assert_eq!(
        store
            .derived(&source, DerivedKind::Thumbnail)
            .await
            .unwrap(),
        None
    );
}

fn person(archive: ArchiveId, n: u8, name: Option<&str>, emails: &[&str]) -> Person {
    Person {
        id: PersonId::from_hash([n; 16]),
        archive_id: archive,
        name: name.map(Into::into),
        email: emails.first().map(|e| (*e).into()),
        phone: None,
        emails: emails.iter().map(|e| (*e).into()).collect(),
        mail_emails: emails.iter().map(|e| (*e).into()).collect(),
        phones: Vec::new(),
        cards: Vec::new(),
        links: Vec::new(),
        mail: None,
        differences: false,
        removed: false,
    }
}

fn mail_from(archive: ArchiveId, n: u32, secs: i64, from: (&str, &str), to: &[&str]) -> NewMessage {
    let mut message = message(archive, 9000 + n, secs);
    message.from = vec![Address {
        name: Some(from.0.into()),
        email: from.1.into(),
    }];
    message.to = to
        .iter()
        .map(|e| Address {
            name: None,
            email: (*e).into(),
        })
        .collect();
    message
}

async fn people(store: &dyn Store) {
    let archive = archive(store).await;
    let account = store
        .create_account(NewAccount {
            archive_id: archive,
            kind: AccountKind::Imap,
            name: "Work".into(),
            address: Some("Me@Example.com".into()),
            settings: json!({}),
            sealed_secret: None,
            enabled: true,
        })
        .await
        .unwrap();
    let sent = store
        .upsert_folder(account.id, "Sent", Some(FolderRole::Sent))
        .await
        .unwrap();
    assert_eq!(
        store.mail_addresses(archive).await.unwrap().addresses,
        Vec::new()
    );

    let first = store
        .insert_message(mail_from(
            archive,
            1,
            1_700_000_000,
            ("Ada L", "me@example.com"),
            &["Ada@Example.com", "cleo@example.org"],
        ))
        .await
        .unwrap();
    let mut reply = mail_from(
        archive,
        2,
        1_735_700_000,
        ("Ada Lovelace", "ada@example.com"),
        &["me@example.com"],
    );
    reply.parent_ids = vec!["m9001@example.com".into()];
    reply.attachments = vec![
        Attachment {
            ordinal: 0,
            blob_id: BlobId::of(b"plan.pdf"),
            filename: Some("plan.pdf".into()),
            content_type: "application/pdf".into(),
            size: 8,
            content_id: None,
            inline: false,
        },
        Attachment {
            ordinal: 1,
            blob_id: BlobId::of(b"BEGIN:VCALENDAR"),
            filename: None,
            content_type: "text/calendar".into(),
            size: 15,
            content_id: None,
            inline: false,
        },
    ];
    let reply = store.insert_message(reply).await.unwrap();
    assert_eq!(reply.thread_id, first.thread_id);
    let other = store
        .insert_message(mail_from(
            archive,
            3,
            1_735_800_000,
            ("Alias", "me+alias@example.com"),
            &["news@example.net"],
        ))
        .await
        .unwrap();
    store
        .record_location(other.message_id, sent.id, Some(1), &[], Timestamp::now())
        .await
        .unwrap();
    store
        .insert_message(mail_from(
            archive,
            4,
            1_735_900_000,
            ("News", "news@example.net"),
            &["me@example.com"],
        ))
        .await
        .unwrap();

    let found = store.mail_addresses(archive).await.unwrap();
    assert_eq!(
        found.own,
        vec!["me+alias@example.com".to_owned(), "me@example.com".into()],
        "the account address and every From of sent mail are the owner's"
    );
    let ada = found
        .addresses
        .iter()
        .find(|a| a.email == "ada@example.com")
        .expect("addresses are lowercased");
    assert_eq!(
        (ada.messages, ada.from_them, ada.from_me),
        (2, 1, 1),
        "{ada:?}"
    );
    assert_eq!(ada.first_at, Timestamp::from_secs(1_700_000_000));
    assert_eq!(ada.last_at, Timestamp::from_secs(1_735_700_000));
    assert_eq!(ada.name.as_deref(), Some("Ada Lovelace"));
    let news = found
        .addresses
        .iter()
        .find(|a| a.email == "news@example.net")
        .unwrap();
    assert_eq!(
        (news.messages, news.from_them, news.from_me),
        (2, 1, 1),
        "mail from a sent folder counts as the owner's"
    );
    let emails: Vec<&str> = found.addresses.iter().map(|a| a.email.as_str()).collect();
    let mut sorted = emails.clone();
    sorted.sort_unstable();
    assert_eq!(emails, sorted);

    let book = book(store, archive, "people.vcf").await;
    let card = store
        .record_version(item_version(
            book,
            "ada",
            "BEGIN:VCARD ada",
            ItemKind::Contact,
            pim_index(Some("Ada"), None),
        ))
        .await
        .unwrap();
    let mut meeting = pim_index(Some("Review"), Some(1_735_000_000));
    meeting.emails = vec!["bob@example.org".into(), "ada@example.com".into()];
    let meeting = store
        .record_version(item_version(
            book,
            "review",
            "BEGIN:VCALENDAR review",
            ItemKind::Event,
            meeting,
        ))
        .await
        .unwrap();
    let mut undated = pim_index(Some("Someday"), None);
    undated.emails = vec!["ada@example.com".into()];
    let undated = store
        .record_version(item_version(
            book,
            "someday",
            "BEGIN:VCALENDAR someday",
            ItemKind::Event,
            undated,
        ))
        .await
        .unwrap();
    let mut later = pim_index(Some("Lunch"), Some(1_736_000_000));
    later.emails = vec!["cleo@example.org".into()];
    store
        .record_version(item_version(
            book,
            "lunch",
            "BEGIN:VCALENDAR lunch",
            ItemKind::Event,
            later,
        ))
        .await
        .unwrap();
    let meetings: Vec<ItemId> = store
        .items_by_email(archive, ItemKind::Event, &["ADA@example.com".into()], 10)
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.item.id)
        .collect();
    assert_eq!(meetings, vec![meeting.item_id, undated.item_id]);
    assert!(
        store
            .items_by_email(archive, ItemKind::Event, &[], 10)
            .await
            .unwrap()
            .is_empty()
    );

    let talk = store
        .correspondence(archive, &["ADA@example.com".into()], 10, 10)
        .await
        .unwrap();
    assert_eq!(
        (talk.messages, talk.from_them, talk.to_them),
        (2, 1, 1),
        "{talk:?}"
    );
    assert_eq!(talk.first_at, Some(Timestamp::from_secs(1_700_000_000)));
    assert_eq!(talk.last_at, Some(Timestamp::from_secs(1_735_700_000)));
    assert_eq!(
        talk.years,
        vec![
            YearCount {
                year: 2023,
                messages: 1
            },
            YearCount {
                year: 2025,
                messages: 1
            }
        ]
    );
    assert_eq!(talk.threads.len(), 1);
    assert_eq!(talk.threads[0].thread_id, first.thread_id);
    assert_eq!(talk.threads[0].messages, 2);
    assert_eq!(talk.threads[0].latest, reply.message_id);
    assert_eq!(
        talk.attachments.len(),
        1,
        "calendar parts are meetings, not files"
    );
    assert_eq!(talk.attachments[0].message_id, reply.message_id);
    assert!(talk.attachments[0].from_them);
    assert_eq!(
        talk.attachments[0].attachment.filename.as_deref(),
        Some("plan.pdf")
    );
    assert_eq!(
        store.correspondence(archive, &[], 10, 10).await.unwrap(),
        Correspondence::default()
    );
    let both = store
        .correspondence(
            archive,
            &["ada@example.com".into(), "cleo@example.org".into()],
            0,
            0,
        )
        .await
        .unwrap();
    assert_eq!(
        both.messages, 2,
        "a message with both addresses counts once"
    );
    assert!(both.threads.is_empty() && both.attachments.is_empty());

    let mut ada = person(archive, 1, Some("Ada Lovelace"), &["ada@example.com"]);
    ada.cards = vec![PersonCard {
        item_id: card.item_id,
        collection_id: book,
        removed: false,
    }];
    ada.phones = vec!["+15550100".into()];
    ada.differences = true;
    ada.mail = Some(PersonMail {
        messages: 2,
        from_them: 1,
        from_me: 1,
        first_at: Timestamp::from_secs(1_700_000_000),
        last_at: Timestamp::from_secs(1_735_700_000),
    });
    let cleo = person(archive, 2, Some("cleo"), &["cleo@example.org"]);
    let nameless = person(archive, 3, None, &["x@example.com"]);
    let bob = person(archive, 4, Some("Bob"), &["bob@example.org"]);
    store
        .replace_people(
            archive,
            &[cleo.clone(), nameless.clone(), ada.clone(), bob.clone()],
        )
        .await
        .unwrap();
    let query = |text: Option<&str>, source: Option<PersonSource>, differences: bool| PersonQuery {
        archive_id: archive,
        text: text.map(Into::into),
        source,
        differences,
        after: None,
        limit: 50,
    };
    let names = |page: &Page<Person>| -> Vec<Option<String>> {
        page.items.iter().map(|p| p.name.clone()).collect()
    };
    let all = store.list_people(&query(None, None, false)).await.unwrap();
    assert_eq!(
        names(&all),
        vec![
            Some("Ada Lovelace".into()),
            Some("Bob".into()),
            Some("cleo".into()),
            None
        ],
        "by name without regard to case, nameless last"
    );
    assert_eq!(all.items[0], ada, "a person reads back as stored");
    assert_eq!(
        store.count_people(&query(None, None, false)).await.unwrap(),
        4
    );
    let mut paged = query(None, None, false);
    paged.limit = 3;
    let page = store.list_people(&paged).await.unwrap();
    assert_eq!(page.items.len(), 3);
    paged.after = page.next.clone();
    let rest = store.list_people(&paged).await.unwrap();
    assert_eq!(names(&rest), vec![None]);
    assert_eq!(rest.next, None);
    assert!(matches!(
        store
            .list_people(&PersonQuery {
                after: Some(PageCursor("nope".into())),
                ..query(None, None, false)
            })
            .await,
        Err(StoreError::Invalid(_))
    ));
    for (text, expected) in [
        ("LOVE", vec![Some("Ada Lovelace".to_owned())]),
        ("example.org", vec![Some("Bob".into()), Some("cleo".into())]),
        ("+1555", vec![Some("Ada Lovelace".into())]),
    ] {
        let page = store
            .list_people(&query(Some(text), None, false))
            .await
            .unwrap();
        assert_eq!(names(&page), expected, "{text}");
    }
    assert_eq!(
        names(
            &store
                .list_people(&query(None, Some(PersonSource::Contacts), false))
                .await
                .unwrap()
        ),
        vec![Some("Ada Lovelace".to_owned())]
    );
    assert_eq!(
        store
            .count_people(&query(None, Some(PersonSource::Mail), false))
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        store
            .count_people(&query(None, Some(PersonSource::Collection(book)), false))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .count_people(&query(
                None,
                Some(PersonSource::Collection(CollectionId::new())),
                false
            ))
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        names(&store.list_people(&query(None, None, true)).await.unwrap()),
        vec![Some("Ada Lovelace".to_owned())]
    );
    assert_eq!(store.get_person(ada.id).await.unwrap(), Some(ada.clone()));
    assert_eq!(
        store
            .people_by_email(
                archive,
                &[
                    "Bob@Example.org".into(),
                    "nobody@example.com".into(),
                    "ada@example.com".into()
                ]
            )
            .await
            .unwrap(),
        vec![
            ("Bob@Example.org".to_owned(), bob.id),
            ("ada@example.com".to_owned(), ada.id)
        ]
    );

    let other = store.create_archive("Other").await.unwrap().id;
    assert!(matches!(
        store
            .replace_people(other, std::slice::from_ref(&cleo))
            .await,
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        store.replace_people(ArchiveId::new(), &[]).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(
        store
            .people_by_email(other, &["bob@example.org".into()])
            .await
            .unwrap()
            .is_empty(),
        "people belong to their archive"
    );

    store
        .replace_people(archive, std::slice::from_ref(&bob))
        .await
        .unwrap();
    assert_eq!(store.get_person(ada.id).await.unwrap(), None);
    assert_eq!(
        store.count_people(&query(None, None, false)).await.unwrap(),
        1
    );
    assert!(
        store
            .people_by_email(archive, &["ada@example.com".into()])
            .await
            .unwrap()
            .is_empty(),
        "replaced people leave no addresses behind"
    );

    store.delete_archive(archive).await.unwrap();
    assert_eq!(store.get_person(bob.id).await.unwrap(), None);
    assert_eq!(
        store.mail_addresses(archive).await.unwrap(),
        MailAddresses::default()
    );
    store.delete_archive(other).await.unwrap();
}

async fn person_overrides(store: &dyn Store) {
    let archive = archive(store).await;
    let other = store.create_archive("Other").await.unwrap().id;
    let (a, b, c) = (ItemId::new(), ItemId::new(), ItemId::new());
    let join = PersonOverride {
        id: PersonOverrideId::new(),
        archive_id: archive,
        kind: PersonOverrideKind::Join,
        keys: vec![format!("item:{a}"), "email:dan@example.net".into()],
        from: Vec::new(),
        created_at: Timestamp::from_secs(1_700_000_000),
        created_by: None,
    };
    let split = PersonOverride {
        id: PersonOverrideId::new(),
        archive_id: archive,
        kind: PersonOverrideKind::Split,
        keys: vec![format!("item:{b}")],
        from: vec![format!("item:{a}"), format!("item:{c}")],
        created_at: Timestamp::from_secs(1_700_000_100),
        created_by: None,
    };
    assert!(store.person_overrides(archive).await.unwrap().is_empty());
    store.add_person_override(&split).await.unwrap();
    store.add_person_override(&join).await.unwrap();
    assert!(matches!(
        store.add_person_override(&join).await,
        Err(StoreError::Invalid(_))
    ));
    for bad in [
        PersonOverride {
            id: PersonOverrideId::new(),
            keys: vec![format!("item:{a}")],
            ..join.clone()
        },
        PersonOverride {
            id: PersonOverrideId::new(),
            from: Vec::new(),
            ..split.clone()
        },
        PersonOverride {
            id: PersonOverrideId::new(),
            keys: Vec::new(),
            ..split.clone()
        },
    ] {
        assert!(
            matches!(
                store.add_person_override(&bad).await,
                Err(StoreError::Invalid(_))
            ),
            "{bad:?}"
        );
    }
    assert!(matches!(
        store
            .add_person_override(&PersonOverride {
                id: PersonOverrideId::new(),
                archive_id: ArchiveId::new(),
                ..join.clone()
            })
            .await,
        Err(StoreError::NotFound(_))
    ));
    let mut expected = vec![join.clone(), split.clone()];
    expected.sort_by_key(|o| o.id);
    assert_eq!(store.person_overrides(archive).await.unwrap(), expected);
    assert!(store.person_overrides(other).await.unwrap().is_empty());
    assert_eq!(
        store.get_person_override(split.id).await.unwrap(),
        Some(split.clone())
    );
    assert!(store.remove_person_override(split.id).await.unwrap());
    assert!(!store.remove_person_override(split.id).await.unwrap());
    assert_eq!(store.get_person_override(split.id).await.unwrap(), None);
    assert_eq!(
        store.person_overrides(archive).await.unwrap(),
        vec![join.clone()]
    );

    store.delete_archive(archive).await.unwrap();
    assert_eq!(store.get_person_override(join.id).await.unwrap(), None);
    store.delete_archive(other).await.unwrap();
}

fn instance(rid: Option<&str>, start: i64, end: i64) -> NewOccurrence {
    NewOccurrence {
        recurrence_id: rid.map(Into::into),
        starts_at: Timestamp::from_secs(start),
        ends_at: Timestamp::from_secs(end),
        component: 0,
        start_local: format!("local {start}"),
        end_local: format!("local {end}"),
        time_zone: Some("Europe/Berlin".into()),
        all_day: false,
        floating: false,
        cancelled: false,
        changed: false,
    }
}

async fn calendar(store: &dyn Store) {
    let archive = store.create_archive("Calendar").await.unwrap().id;
    let work = store
        .upsert_collection(NewCollection {
            archive_id: archive,
            account_id: None,
            kind: CollectionKind::Calendar,
            remote_id: "work.ics".into(),
            name: "Work".into(),
            color: Some("#00f".into()),
        })
        .await
        .unwrap()
        .id;
    let home = book(store, archive, "home.ics").await;
    let event = |collection, remote: &str| {
        item_version(
            collection,
            remote,
            remote,
            ItemKind::Event,
            pim_index(Some(remote), Some(1_000)),
        )
    };
    let once = store
        .record_version(event(work, "once"))
        .await
        .unwrap()
        .item_id;
    let weekly = store
        .record_version(event(work, "weekly"))
        .await
        .unwrap()
        .item_id;
    let copy = store
        .record_version(event(home, "copy"))
        .await
        .unwrap()
        .item_id;
    let group = EventGroupId::from_hash([3; 16]);
    let other = EventGroupId::from_hash([4; 16]);
    let series = |item, group, recurring, window: Option<(i64, i64)>, seq| EventSeries {
        item_id: item,
        group_id: group,
        sequence: Some(seq),
        stamp: Some(Timestamp::from_secs(500)),
        starts_at: Some(Timestamp::from_secs(1_000)),
        ends_at: if recurring {
            None
        } else {
            Some(Timestamp::from_secs(1_100))
        },
        recurring,
        window: window.map(|(a, b)| (Timestamp::from_secs(a), Timestamp::from_secs(b))),
    };

    assert!(matches!(
        store
            .replace_series(
                ItemId::new(),
                Some(&series(once, other, false, None, 0)),
                &[]
            )
            .await,
        Err(StoreError::Invalid(_))
    ));
    let unknown = ItemId::new();
    assert!(matches!(
        store
            .replace_series(unknown, Some(&series(unknown, other, false, None, 0)), &[])
            .await,
        Err(StoreError::NotFound(_))
    ));

    store
        .replace_series(
            once,
            Some(&series(once, other, false, None, 0)),
            &[instance(None, 1_000, 1_100)],
        )
        .await
        .unwrap();
    let weekly_series = series(weekly, group, true, Some((0, 10_000)), 2);
    store
        .replace_series(
            weekly,
            Some(&weekly_series),
            &[
                instance(Some("a"), 1_000, 1_050),
                instance(Some("b"), 2_000, 2_000),
                instance(Some("c"), 9_000, 9_050),
            ],
        )
        .await
        .unwrap();
    store
        .replace_series(
            copy,
            Some(&series(copy, group, true, Some((0, 10_000)), 1)),
            &[instance(Some("a"), 1_000, 1_050)],
        )
        .await
        .unwrap();
    assert_eq!(store.series(weekly).await.unwrap(), Some(weekly_series));

    let query = |from: i64, to: i64| OccurrenceQuery {
        archive_id: archive,
        from: Timestamp::from_secs(from),
        to: Timestamp::from_secs(to),
        collections: None,
        removed: false,
    };
    let keys = |found: Vec<Occurrence>| -> Vec<(ItemId, Option<String>)> {
        found
            .into_iter()
            .map(|o| (o.item_id, o.occurrence.recurrence_id))
            .collect()
    };
    let mut first = vec![
        (once, None),
        (weekly, Some("a".to_owned())),
        (copy, Some("a".to_owned())),
    ];
    first[1..].sort();
    let found = store.occurrences(&query(1_040, 1_500)).await.unwrap();
    assert_eq!(found[0].group_id, other);
    assert_eq!(found[0].sequence, Some(0));
    assert_eq!(found[0].stamp, Some(Timestamp::from_secs(500)));
    assert_eq!(found[0].collection_id, work);
    assert!(found.iter().all(|o| !o.removed));
    let mut got = keys(found);
    got[1..].sort();
    assert_eq!(got, first, "overlapping instances, by start");
    assert!(
        store
            .occurrences(&query(1_100, 1_500))
            .await
            .unwrap()
            .is_empty(),
        "an instance ending at the span's start is not in it"
    );
    assert_eq!(
        keys(store.occurrences(&query(2_000, 2_001)).await.unwrap()),
        [(weekly, Some("b".to_owned()))],
        "an instance lasting no time is in a span starting at it"
    );
    assert!(
        store
            .uncovered_series(&query(0, 10_000))
            .await
            .unwrap()
            .is_empty()
    );

    let outside = query(9_000, 20_000);
    assert!(
        store.occurrences(&outside).await.unwrap().is_empty(),
        "stored instances that do not cover the span are not listed"
    );
    let uncovered = store.uncovered_series(&outside).await.unwrap();
    let mut ids: Vec<ItemId> = uncovered.iter().map(|u| u.summary.item.id).collect();
    ids.sort();
    let mut expected = vec![weekly, copy];
    expected.sort();
    assert_eq!(ids, expected);
    assert!(
        uncovered
            .iter()
            .all(|u| u.series.group_id == group && !u.removed && u.summary.index.is_some())
    );
    assert!(
        store
            .uncovered_series(&query(-5_000, 999))
            .await
            .unwrap()
            .is_empty(),
        "series starting after the span do not reach into it"
    );

    let only_home = OccurrenceQuery {
        collections: Some(vec![home]),
        ..query(0, 10_000)
    };
    assert_eq!(
        keys(store.occurrences(&only_home).await.unwrap()),
        [(copy, Some("a".to_owned()))]
    );
    let none = OccurrenceQuery {
        collections: Some(Vec::new()),
        ..query(0, 10_000)
    };
    assert!(store.occurrences(&none).await.unwrap().is_empty());

    store
        .mark_items_removed(home, &["copy".to_owned()], Timestamp::from_secs(3_000))
        .await
        .unwrap();
    assert_eq!(store.occurrences(&query(0, 10_000)).await.unwrap().len(), 4);
    let with_removed = OccurrenceQuery {
        removed: true,
        ..query(0, 10_000)
    };
    let found = store.occurrences(&with_removed).await.unwrap();
    assert_eq!(found.len(), 5);
    assert!(found.iter().any(|o| o.item_id == copy && o.removed));
    store
        .mark_collection_removed(work, Timestamp::from_secs(3_000))
        .await
        .unwrap();
    assert!(
        store
            .occurrences(&query(0, 10_000))
            .await
            .unwrap()
            .is_empty()
    );

    let members = store.group_members(group).await.unwrap();
    let mut member_ids: Vec<ItemId> = members.iter().map(|m| m.summary.item.id).collect();
    member_ids.sort();
    assert_eq!(member_ids, expected, "removed copies are still members");
    let weekly_member = members
        .iter()
        .find(|m| m.summary.item.id == weekly)
        .unwrap();
    assert_eq!(weekly_member.series.sequence, Some(2));
    assert_eq!(
        weekly_member.collection_removed_at,
        Some(Timestamp::from_secs(3_000))
    );
    assert!(
        store
            .group_members(EventGroupId::from_hash([9; 16]))
            .await
            .unwrap()
            .is_empty()
    );

    store.replace_series(copy, None, &[]).await.unwrap();
    assert_eq!(store.series(copy).await.unwrap(), None);
    assert_eq!(store.group_members(group).await.unwrap().len(), 1);
    assert_eq!(store.occurrences(&with_removed).await.unwrap().len(), 4);

    store.delete_archive(archive).await.unwrap();
    assert!(store.group_members(group).await.unwrap().is_empty());
    assert!(store.group_members(other).await.unwrap().is_empty());
}

async fn whole_ledger(store: &dyn Store) -> Vec<LedgerEntry> {
    let mut entries = Vec::new();
    loop {
        let after = entries.last().map_or(0, |e: &LedgerEntry| e.seq);
        let page = store.ledger_entries(after, 7).await.unwrap();
        if page.is_empty() {
            return entries;
        }
        entries.extend(page);
    }
}

async fn ledger(store: &dyn Store) {
    let before = whole_ledger(store).await;
    let mut check = holtmail_ledger::ChainCheck::new();
    for entry in &before {
        check.push(entry);
    }
    let report = check.finish();
    assert!(report.problems.is_empty(), "{report:?}");
    assert_eq!(
        store.ledger_head().await.unwrap().map(|e| (e.seq, e.hash)),
        report.head
    );
    assert!(
        store.ledger_gaps(10).await.unwrap().is_empty(),
        "earlier cases left messages and ledger apart"
    );
    assert_eq!(store.enter_unentered(100).await.unwrap(), 0);

    let archive = store.create_archive("Ledger").await.unwrap().id;
    let new = message(archive, 1, 1_741_600_000);
    let blob = new.blob_id;
    let inserted = store.insert_message(new.clone()).await.unwrap();
    let head = store.ledger_head().await.unwrap().unwrap();
    assert_eq!(head.seq, before.len() as u64 + 1);
    assert_eq!(head.event, LedgerEvent::Archived);
    assert_eq!(head.archive_id, archive);
    assert_eq!(head.message_id, Some(inserted.message_id));
    assert_eq!(head.blob_id, Some(blob));
    assert_eq!(head.actor, None);
    assert_eq!(
        head.prev,
        before.last().map_or(LedgerHash::ZERO, |e| e.hash)
    );
    assert_eq!(head.hash, head.expected_hash());

    store.insert_message(new).await.unwrap();
    assert_eq!(
        store.ledger_head().await.unwrap().unwrap().seq,
        head.seq,
        "the same bytes again were entered twice"
    );

    store.delete_archive(archive).await.unwrap();
    let deleted = store.ledger_head().await.unwrap().unwrap();
    assert_eq!(deleted.seq, head.seq + 1);
    assert_eq!(deleted.event, LedgerEvent::ArchiveDeleted);
    assert_eq!(deleted.archive_id, archive);
    assert_eq!((deleted.message_id, deleted.blob_id), (None, None));
    assert_eq!(deleted.prev, head.hash);
    assert!(store.ledger_gaps(10).await.unwrap().is_empty());
    assert_eq!(
        store.ledger_entries(head.seq - 1, 10).await.unwrap(),
        [head.clone(), deleted],
        "entries of a deleted archive were changed"
    );
}

async fn deletions(store: &dyn Store) {
    let archive = store.create_archive("Deletions").await.unwrap().id;
    let other = store
        .create_archive("Deletions elsewhere")
        .await
        .unwrap()
        .id;
    let account = store
        .create_account(NewAccount {
            archive_id: archive,
            kind: AccountKind::Imap,
            name: "Mail".into(),
            address: None,
            settings: json!({}),
            sealed_secret: None,
            enabled: true,
        })
        .await
        .unwrap();
    let folder = store
        .upsert_folder(account.id, "INBOX", None)
        .await
        .unwrap()
        .id;
    let shared = BlobId::of(b"deletions: a file two messages carry");
    let own = BlobId::of(b"deletions: a file only the first carries");
    let attachment = |ordinal, blob_id| Attachment {
        ordinal,
        blob_id,
        filename: Some(format!("file{ordinal}.txt")),
        content_type: "text/plain".into(),
        size: 10,
        content_id: None,
        inline: false,
    };
    let mut first = message(archive, 9001, 1_741_600_000);
    first.attachments = vec![attachment(0, shared), attachment(1, own)];
    let mut second = message(archive, 9002, 1_741_600_100);
    second.attachments = vec![attachment(0, shared)];
    let third = message(archive, 9003, 1_741_600_200);
    let (first_blob, third_blob) = (first.blob_id, third.blob_id);
    let elsewhere = message(other, 9003, 1_741_600_200);
    let mut ids = Vec::new();
    for (uid, new) in [(11, first), (12, second), (13, third)] {
        let id = store.insert_message(new).await.unwrap().message_id;
        store
            .record_location(id, folder, Some(uid), &[], Timestamp::from_secs(1))
            .await
            .unwrap();
        ids.push(id);
    }
    let kept_elsewhere = store.insert_message(elsewhere).await.unwrap().message_id;
    store
        .set_labels(ids[0], &["Only first".into(), "Both".into()])
        .await
        .unwrap();
    store.set_labels(ids[1], &["Both".into()]).await.unwrap();
    let actor = UserId::new();

    let deleted = store
        .delete_messages(
            archive,
            &[ids[0], ids[2], kept_elsewhere, MessageId::new()],
            Some(actor),
        )
        .await
        .unwrap();
    assert_eq!(
        deleted.messages,
        [ids[0], ids[2]],
        "another archive's message was deleted"
    );
    assert!(store.get_message(ids[0]).await.unwrap().is_none());
    assert!(store.get_message(ids[1]).await.unwrap().is_some());
    assert!(store.get_message(kept_elsewhere).await.unwrap().is_some());
    assert!(deleted.orphans.contains(&first_blob) && deleted.orphans.contains(&own));
    assert!(
        !deleted.orphans.contains(&shared),
        "a file another message carries"
    );
    assert!(
        !deleted.orphans.contains(&third_blob),
        "bytes another archive keeps"
    );
    assert!(store.locations(ids[0]).await.unwrap().is_empty());
    let labels: Vec<String> = store
        .list_labels(archive)
        .await
        .unwrap()
        .into_iter()
        .map(|l| l.name)
        .collect();
    assert_eq!(labels, ["Both"]);

    assert!(store.is_deleted(archive, &first_blob).await.unwrap());
    assert!(!store.is_deleted(other, &third_blob).await.unwrap());
    assert!(!store.is_deleted(archive, &shared).await.unwrap());
    assert_eq!(store.folder_uids(folder).await.unwrap(), [11, 12, 13]);
    assert_eq!(store.message_at_uid(folder, 11).await.unwrap(), None);

    let head = store.ledger_head().await.unwrap().unwrap();
    assert_eq!(head.event, LedgerEvent::Deleted);
    assert_eq!(head.message_id, Some(ids[2]));
    assert_eq!(head.blob_id, Some(third_blob));
    assert_eq!(head.actor, Some(actor));
    assert!(store.ledger_gaps(10).await.unwrap().is_empty());

    let again = store
        .delete_messages(archive, &[ids[0]], None)
        .await
        .unwrap();
    assert_eq!(again, DeletedMessages::default());
    assert_eq!(store.ledger_head().await.unwrap().unwrap().seq, head.seq);

    store
        .mark_removed(folder, &[11], Timestamp::from_secs(2))
        .await
        .unwrap();
    store.record_deleted_location(folder, 40).await.unwrap();
    store.record_deleted_location(folder, 40).await.unwrap();
    assert_eq!(store.folder_uids(folder).await.unwrap(), [12, 13, 40]);
    store.forget_uids(folder).await.unwrap();
    assert!(store.folder_uids(folder).await.unwrap().is_empty());
    assert!(
        store.is_deleted(archive, &first_blob).await.unwrap(),
        "forgetting UIDs forgot the bytes"
    );

    store.delete_archive(archive).await.unwrap();
    assert!(!store.is_deleted(archive, &first_blob).await.unwrap());
    store.delete_archive(other).await.unwrap();
}

async fn webhooks(store: &dyn Store) {
    let hook = Webhook {
        id: holtmail_core::WebhookId::new(),
        url: "https://hooks.example.com/holtmail".into(),
        events: vec![
            "task.finished".into(),
            "messages.deleted".into(),
            "task.finished".into(),
        ],
        sealed_secret: vec![1, 2, 3],
        enabled: true,
        created_at: Timestamp::from_secs(10),
    };
    store.create_webhook(&hook).await.unwrap();
    assert!(matches!(
        store.create_webhook(&hook).await,
        Err(StoreError::Invalid(_))
    ));
    let stored = store.get_webhook(hook.id).await.unwrap().unwrap();
    assert_eq!(stored.events, ["messages.deleted", "task.finished"]);
    assert_eq!(stored.sealed_secret, [1, 2, 3]);
    let mut changed = stored.clone();
    changed.enabled = false;
    changed.url = "http://127.0.0.1:9/hook".into();
    store.update_webhook(&changed).await.unwrap();
    assert_eq!(store.list_webhooks().await.unwrap(), [changed.clone()]);
    let mut unknown = changed.clone();
    unknown.id = holtmail_core::WebhookId::new();
    assert!(matches!(
        store.update_webhook(&unknown).await,
        Err(StoreError::NotFound(_))
    ));

    let new = |n: i64| NewDelivery {
        webhook_id: hook.id,
        event: "task.finished".into(),
        event_id: format!("e{n}"),
        payload: format!("{{\"n\":{n}}}").into_bytes(),
        at: Timestamp::from_secs(100 + n),
    };
    store
        .enqueue_deliveries(&[new(1), new(2), new(3)])
        .await
        .unwrap();
    let orphan = NewDelivery {
        webhook_id: holtmail_core::WebhookId::new(),
        ..new(4)
    };
    assert!(matches!(
        store.enqueue_deliveries(&[new(5), orphan]).await,
        Err(StoreError::NotFound(_))
    ));
    let due = store
        .due_deliveries(Timestamp::from_secs(102), 10)
        .await
        .unwrap();
    assert_eq!(
        due.iter().map(|d| d.event_id.as_str()).collect::<Vec<_>>(),
        ["e1", "e2"],
        "a delivery for an unknown webhook was half added, or not in order"
    );
    assert!(due[0].id < due[1].id);
    assert_eq!(due[0].payload, br#"{"n":1}"#);
    assert_eq!(
        (due[0].attempts, due[0].next_at),
        (0, Some(Timestamp::from_secs(101)))
    );

    store
        .record_attempt(
            due[0].id,
            Some(200),
            None,
            None,
            Some(Timestamp::from_secs(103)),
        )
        .await
        .unwrap();
    store
        .record_attempt(
            due[1].id,
            None,
            Some("unreachable"),
            Some(Timestamp::from_secs(500)),
            None,
        )
        .await
        .unwrap();
    store
        .record_attempt(9_999_999, Some(200), None, None, None)
        .await
        .unwrap();
    let due = store
        .due_deliveries(Timestamp::from_secs(400), 10)
        .await
        .unwrap();
    assert_eq!(
        due.iter().map(|d| d.event_id.as_str()).collect::<Vec<_>>(),
        ["e3"]
    );
    let recent = store.recent_deliveries(hook.id, 10).await.unwrap();
    assert_eq!(
        recent
            .iter()
            .map(|d| d.event_id.as_str())
            .collect::<Vec<_>>(),
        ["e3", "e2", "e1"]
    );
    assert_eq!(recent[2].last_status, Some(200));
    assert_eq!(recent[2].delivered_at, Some(Timestamp::from_secs(103)));
    assert_eq!(recent[1].attempts, 1);
    assert_eq!(recent[1].last_error.as_deref(), Some("unreachable"));
    assert_eq!(recent[1].next_at, Some(Timestamp::from_secs(500)));

    store
        .record_attempt(recent[0].id, Some(500), None, None, None)
        .await
        .unwrap();
    assert_eq!(store.prune_deliveries(1).await.unwrap(), 1);
    let kept = store.recent_deliveries(hook.id, 10).await.unwrap();
    assert_eq!(
        kept.iter().map(|d| d.event_id.as_str()).collect::<Vec<_>>(),
        ["e3", "e2"],
        "pruning dropped a waiting delivery or kept too many"
    );

    store.delete_webhook(hook.id).await.unwrap();
    assert!(store.get_webhook(hook.id).await.unwrap().is_none());
    assert!(
        store
            .recent_deliveries(hook.id, 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .due_deliveries(Timestamp::from_secs(10_000), 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        store.delete_webhook(hook.id).await,
        Err(StoreError::NotFound(_))
    ));
}
