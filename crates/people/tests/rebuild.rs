//! Rebuilding people from stored contacts and mail.

use holtmail_blob::BlobId;
use holtmail_core::{Address, Timestamp};
use holtmail_pim::{Contact, Email, Normalized};
use holtmail_store::{
    CollectionKind, ItemKind, NewCollection, NewItemVersion, NewMessage, PersonQuery, PimIndex,
    Store,
};
use holtmail_store_sqlite::SqliteStore;

fn contact(name: &str, email: &str) -> PimIndex {
    let normalized = Normalized::Contact(Box::new(Contact {
        full_name: Some(name.into()),
        emails: vec![Email {
            address: email.into(),
            normalized: holtmail_pim::normalize_email(email),
            kinds: Vec::new(),
        }],
        ..Contact::default()
    }));
    PimIndex {
        version: 1,
        readable: true,
        title: Some(name.into()),
        starts_at: None,
        ends_at: None,
        all_day: false,
        time_zone: None,
        recurring: false,
        sequence: None,
        uid: None,
        organizer: None,
        emails: normalized.emails(),
        phones: normalized.phones(),
        data: serde_json::to_value(&normalized).unwrap(),
    }
}

fn message(archive: holtmail_core::ArchiveId, n: u32, from: &str, to: &str) -> NewMessage {
    NewMessage {
        archive_id: archive,
        blob_id: BlobId::of(format!("message {n}").as_bytes()),
        size: 10,
        rfc_message_id: Some(format!("m{n}@example.com")),
        parent_ids: Vec::new(),
        subject: Some(format!("Subject {n}")),
        from: vec![Address {
            name: Some(format!("Sender {n}")),
            email: from.into(),
        }],
        to: vec![Address {
            name: None,
            email: to.into(),
        }],
        cc: Vec::new(),
        bcc: Vec::new(),
        sent_at: Some(Timestamp::from_secs(1_700_000_000 + i64::from(n))),
        received_at: None,
        preview: String::new(),
        attachments: Vec::new(),
    }
}

#[tokio::test]
async fn rebuilds_the_same_people_from_contacts_and_mail() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("holtmail.db"))
        .await
        .unwrap();
    let store: &dyn Store = &store;
    let archive = store.create_archive("Home").await.unwrap().id;
    let book = store
        .upsert_collection(NewCollection {
            archive_id: archive,
            account_id: None,
            kind: CollectionKind::Contacts,
            remote_id: "people.vcf".into(),
            name: "people".into(),
            color: None,
        })
        .await
        .unwrap()
        .id;
    for (remote, name, email) in [
        ("a", "Ada Lovelace", "ada@example.com"),
        ("b", "Ada", "ADA@example.com"),
    ] {
        store
            .record_version(NewItemVersion {
                collection_id: book,
                remote_id: remote.into(),
                kind: ItemKind::Contact,
                key: None,
                blob_id: BlobId::of(remote.as_bytes()),
                content_type: "text/vcard".into(),
                etag: None,
                seen_at: Timestamp::from_secs(1_700_000_000),
                photo_blob_id: None,
                index: contact(name, email),
            })
            .await
            .unwrap();
    }
    for n in 0..2 {
        store
            .insert_message(message(archive, n, "bob@example.org", "ada@example.com"))
            .await
            .unwrap();
    }
    store
        .insert_message(message(archive, 9, "once@example.net", "ada@example.com"))
        .await
        .unwrap();

    let rebuilt = holtmail_people::rebuild(store, archive).await.unwrap();
    assert_eq!(rebuilt.people, 2, "{rebuilt:?}");
    assert_eq!((rebuilt.with_cards, rebuilt.from_mail), (1, 1));
    let query = PersonQuery {
        archive_id: archive,
        text: None,
        source: None,
        differences: false,
        after: None,
        limit: 50,
    };
    let first = store.list_people(&query).await.unwrap().items;
    let ada = first.iter().find(|p| p.cards.len() == 2).unwrap();
    assert_eq!(ada.mail.unwrap().messages, 3);
    assert_eq!(ada.links.len(), 1);
    assert!(ada.differences, "the cards give different names");

    holtmail_people::rebuild_all(store).await.unwrap();
    assert_eq!(store.list_people(&query).await.unwrap().items, first);
}
