use holtmail_blob::BlobId;
use holtmail_core::{ArchiveId, CollectionId, ItemChangeId, ItemId, Timestamp};
use serde_json::json;

use crate::*;

pub(super) async fn run(store: &dyn Store) {
    modes(store).await;
    recording(store).await;
    creates(store).await;
    resolving(store).await;
    upstream_flags(store).await;
    writeback(store).await;
    successors(store).await;
    log_and_blobs(store).await;
    schedule_tags(store).await;
    schedule_inbox(store).await;
}

fn t(secs: i64) -> Timestamp {
    Timestamp::from_secs(1_795_000_000 + secs)
}

struct Setup {
    archive: ArchiveId,
    calendar: CollectionId,
    imported: CollectionId,
    item: ItemId,
}

fn upstream(collection: CollectionId, remote: &str, bytes: &str) -> NewItemVersion {
    NewItemVersion {
        collection_id: collection,
        remote_id: remote.into(),
        kind: ItemKind::Event,
        key: Some(format!("{remote}@example.com")),
        blob_id: BlobId::of(bytes.as_bytes()),
        content_type: "text/calendar".into(),
        etag: Some(format!("\"{bytes}\"")),
        seen_at: t(0),
        photo_blob_id: None,
        index: super::pim_index(Some(bytes), Some(1_795_000_000)),
    }
}

async fn setup(store: &dyn Store, name: &str) -> Setup {
    let archive = store.create_archive(name).await.unwrap().id;
    let account = store
        .create_account(NewAccount {
            archive_id: archive,
            kind: AccountKind::Imap,
            name: name.into(),
            address: Some("ada@example.com".into()),
            settings: json!({}),
            sealed_secret: None,
            enabled: false,
        })
        .await
        .unwrap()
        .id;
    let new = |account, remote: &str| NewCollection {
        archive_id: archive,
        account_id: account,
        kind: CollectionKind::Calendar,
        remote_id: remote.into(),
        name: remote.into(),
        color: None,
    };
    let calendar = store
        .upsert_collection(new(Some(account), "/cal/"))
        .await
        .unwrap();
    assert_eq!(
        calendar.write_mode,
        WriteMode::Held,
        "upstream collections are held"
    );
    let imported = store
        .upsert_collection(new(None, "work.ics"))
        .await
        .unwrap();
    assert_eq!(
        imported.write_mode,
        WriteMode::ReadOnly,
        "collections no account owns are read-only"
    );
    let item = store
        .record_version(upstream(calendar.id, "/cal/a.ics", &format!("{name} a v1")))
        .await
        .unwrap()
        .item_id;
    Setup {
        archive,
        calendar: calendar.id,
        imported: imported.id,
        item,
    }
}

fn content(bytes: &str) -> Option<ChangeContent> {
    Some(ChangeContent {
        blob_id: BlobId::of(bytes.as_bytes()),
        content_type: "text/calendar".into(),
    })
}

fn update(item: ItemId, base: u32, bytes: &str, class: ChangeClass, at: i64) -> NewItemChange {
    NewItemChange {
        target: ChangeTarget::Item { item },
        op: ChangeOp::Update,
        class,
        content: content(bytes),
        patch: Some(json!({ "summary": bytes })),
        base_version: Some(base),
        origin: ChangeOrigin::Dav,
        user_id: None,
        device: Some("phone".into()),
        at: t(at),
        not_before: None,
        messages: Vec::new(),
        schedule_tag: None,
    }
}

fn at_href(collection: CollectionId, href: &str, op: ChangeOp, at: i64) -> NewItemChange {
    NewItemChange {
        target: ChangeTarget::Href {
            collection,
            href: href.into(),
        },
        op,
        class: match op {
            ChangeOp::Create => ChangeClass::Create,
            ChangeOp::Update => ChangeClass::Edit,
            ChangeOp::Delete => ChangeClass::Delete,
        },
        content: (op != ChangeOp::Delete).then(|| content(&format!("{href} {at}")).unwrap()),
        patch: None,
        base_version: None,
        origin: ChangeOrigin::Api,
        user_id: None,
        device: None,
        at: t(at),
        not_before: None,
        messages: Vec::new(),
        schedule_tag: None,
    }
}

async fn record(store: &dyn Store, change: &NewItemChange, expect: Expect) -> ItemChange {
    match store.record_change(change, expect).await.unwrap() {
        RecordedChange::Recorded(change) => *change,
        other => panic!("expected a recorded change, got {other:?}"),
    }
}

fn item(item: ItemId) -> ChangeTarget {
    ChangeTarget::Item { item }
}

async fn modes(store: &dyn Store) {
    let s = setup(store, "Edit modes").await;
    let import = store
        .create_account(NewAccount {
            archive_id: s.archive,
            kind: AccountKind::Import,
            name: "Files".into(),
            address: None,
            settings: json!({}),
            sealed_secret: None,
            enabled: false,
        })
        .await
        .unwrap()
        .id;
    let file = store
        .upsert_collection(NewCollection {
            archive_id: s.archive,
            account_id: Some(import),
            kind: CollectionKind::Contacts,
            remote_id: "people.vcf".into(),
            name: "people".into(),
            color: None,
        })
        .await
        .unwrap();
    assert_eq!(
        file.write_mode,
        WriteMode::ReadOnly,
        "imported files are read-only"
    );
    assert!(matches!(
        store.set_write_mode(file.id, WriteMode::Held).await,
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        store.set_write_mode(s.imported, WriteMode::Held).await,
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(
        store
            .set_write_mode(s.imported, WriteMode::ReadOnly)
            .await
            .unwrap()
            .write_mode,
        WriteMode::ReadOnly
    );
    let fresh = store.get_collection(s.calendar).await.unwrap().unwrap();
    assert_eq!(
        (fresh.write_mode_changed_at, fresh.previous_write_mode),
        (None, None),
        "a mode never changed has no change time"
    );
    let before = Timestamp::now();
    let changed = store
        .set_write_mode(s.calendar, WriteMode::WriteThrough)
        .await
        .unwrap();
    assert_eq!(changed.write_mode, WriteMode::WriteThrough);
    assert_eq!(changed.previous_write_mode, Some(fresh.write_mode));
    let at = changed.write_mode_changed_at.expect("the change time");
    assert!(at >= before);
    let same = store
        .set_write_mode(s.calendar, WriteMode::WriteThrough)
        .await
        .unwrap();
    assert_eq!(
        (same.write_mode_changed_at, same.previous_write_mode),
        (Some(at), Some(fresh.write_mode)),
        "the same mode again changes nothing"
    );
    assert_eq!(
        store
            .get_collection(s.calendar)
            .await
            .unwrap()
            .unwrap()
            .write_mode,
        WriteMode::WriteThrough
    );
    assert!(matches!(
        store
            .set_write_mode(CollectionId::new(), WriteMode::Held)
            .await,
        Err(StoreError::NotFound(_))
    ));
}

async fn recording(store: &dyn Store) {
    let s = setup(store, "Edit recording").await;
    let seen = store.effective(&item(s.item)).await.unwrap().unwrap();
    assert!(seen.served);
    assert_eq!(seen.href, "/cal/a.ics");
    assert_eq!(
        seen.state,
        ServedState {
            version: Some(1),
            change: None
        }
    );
    assert_eq!(
        seen.latest.as_ref().unwrap().origin,
        VersionOrigin::Upstream
    );
    assert!(seen.change.is_none());

    let first = record(
        store,
        &update(s.item, 1, "edit one", ChangeClass::Edit, 1),
        Expect::State(seen.state),
    )
    .await;
    assert_eq!(first.state, ItemChangeState::Held);
    assert_eq!(first.item_id, Some(s.item));
    assert_eq!(first.href, None);
    assert_eq!(first.base_etag.as_deref(), Some("\"Edit recording a v1\""));
    assert_eq!(first.supersedes, None);
    assert_eq!(first.device.as_deref(), Some("phone"));
    assert_eq!(first.patch, Some(json!({ "summary": "edit one" })));
    assert!(!first.flagged());

    let stale = store
        .record_change(
            &update(s.item, 1, "edit stale", ChangeClass::Edit, 2),
            Expect::State(seen.state),
        )
        .await
        .unwrap();
    assert_eq!(
        stale,
        RecordedChange::PreconditionFailed {
            current: ServedState {
                version: Some(1),
                change: Some(first.id)
            }
        },
        "a stale state records nothing"
    );

    let second = record(
        store,
        &update(s.item, 1, "edit two", ChangeClass::Personal, 3),
        Expect::Any,
    )
    .await;
    assert_eq!(second.supersedes, Some(first.id));
    assert_eq!(
        store.get_change(first.id).await.unwrap().unwrap().state,
        ItemChangeState::Superseded
    );
    let now = store.effective(&item(s.item)).await.unwrap().unwrap();
    assert_eq!(now.change.as_ref().map(|c| c.id), Some(second.id));
    let history = store.changes_of(&item(s.item)).await.unwrap();
    assert_eq!(
        history.iter().map(|c| c.id).collect::<Vec<_>>(),
        vec![first.id, second.id]
    );
    assert_eq!(
        store
            .changes_of(&ChangeTarget::Href {
                collection: s.calendar,
                href: "/cal/a.ics".into()
            })
            .await
            .unwrap()
            .len(),
        2,
        "an href names the item it is served under"
    );

    let refused = [
        update(s.item, 1, "x", ChangeClass::Create, 4),
        NewItemChange {
            op: ChangeOp::Delete,
            class: ChangeClass::Delete,
            ..update(s.item, 1, "x", ChangeClass::Edit, 4)
        },
        NewItemChange {
            content: None,
            ..update(s.item, 1, "x", ChangeClass::Edit, 4)
        },
        NewItemChange {
            base_version: None,
            ..update(s.item, 1, "x", ChangeClass::Edit, 4)
        },
        update(s.item, 9, "x", ChangeClass::Edit, 4),
        NewItemChange {
            op: ChangeOp::Create,
            class: ChangeClass::Create,
            base_version: None,
            ..update(s.item, 1, "x", ChangeClass::Edit, 4)
        },
    ];
    for change in &refused {
        assert!(
            matches!(
                store.record_change(change, Expect::Any).await,
                Err(StoreError::Invalid(_))
            ),
            "{change:?} is refused"
        );
    }
    assert!(matches!(
        store
            .record_change(
                &update(ItemId::new(), 1, "x", ChangeClass::Edit, 4),
                Expect::Any
            )
            .await,
        Err(StoreError::NotFound(_))
    ));
    let imported = store
        .record_version(upstream(s.imported, "b", "imported b"))
        .await
        .unwrap()
        .item_id;
    assert!(
        matches!(
            store
                .record_change(&update(imported, 1, "x", ChangeClass::Edit, 4), Expect::Any)
                .await,
            Err(StoreError::Invalid(_))
        ),
        "a collection no account owns takes no changes"
    );
    assert_eq!(
        store.changes_of(&item(s.item)).await.unwrap().len(),
        2,
        "refused changes left nothing"
    );
}

async fn creates(store: &dyn Store) {
    let s = setup(store, "Edit creates").await;
    let href = |h: &str| ChangeTarget::Href {
        collection: s.calendar,
        href: h.into(),
    };
    assert_eq!(store.effective(&href("new.ics")).await.unwrap(), None);
    let created = record(
        store,
        &at_href(s.calendar, "new.ics", ChangeOp::Create, 1),
        Expect::Absent,
    )
    .await;
    assert_eq!(created.state, ItemChangeState::Held);
    assert_eq!(created.item_id, None);
    assert_eq!(created.href.as_deref(), Some("new.ics"));
    let served = store.effective(&href("new.ics")).await.unwrap().unwrap();
    assert!(served.served);
    assert_eq!(served.href, "new.ics");
    assert!(served.item.is_none());
    assert_eq!(
        served.state,
        ServedState {
            version: None,
            change: Some(created.id)
        }
    );

    for taken in ["new.ics", "/cal/a.ics"] {
        assert!(
            matches!(
                store
                    .record_change(
                        &at_href(s.calendar, taken, ChangeOp::Create, 2),
                        Expect::Absent
                    )
                    .await
                    .unwrap(),
                RecordedChange::PreconditionFailed { .. }
            ),
            "{taken} is taken"
        );
    }
    assert!(matches!(
        store
            .record_change(
                &at_href(s.calendar, "new.ics", ChangeOp::Create, 2),
                Expect::Any
            )
            .await,
        Err(StoreError::Invalid(_))
    ));

    let updated = record(
        store,
        &at_href(s.calendar, "new.ics", ChangeOp::Update, 3),
        Expect::State(served.state),
    )
    .await;
    assert_eq!(updated.supersedes, Some(created.id));
    let deleted = record(
        store,
        &at_href(s.calendar, "new.ics", ChangeOp::Delete, 4),
        Expect::Any,
    )
    .await;
    assert_eq!(
        deleted.state,
        ItemChangeState::Applied,
        "deleting a create that never left is done at once"
    );
    let gone = store.effective(&href("new.ics")).await.unwrap().unwrap();
    assert!(!gone.served);
    assert_eq!(gone.change, None);
    assert!(matches!(
        store
            .record_change(
                &at_href(s.calendar, "new.ics", ChangeOp::Update, 5),
                Expect::Any
            )
            .await,
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        store
            .record_change(
                &at_href(s.calendar, "missing.ics", ChangeOp::Delete, 5),
                Expect::Any
            )
            .await,
        Err(StoreError::Invalid(_))
    ));
    let again = record(
        store,
        &at_href(s.calendar, "new.ics", ChangeOp::Create, 6),
        Expect::Absent,
    )
    .await;
    assert_eq!(again.supersedes, None);
    assert_eq!(
        store.changes_of(&href("new.ics")).await.unwrap().len(),
        4,
        "the history keeps every row"
    );
    assert!(matches!(
        store
            .record_change(
                &at_href(CollectionId::new(), "x.ics", ChangeOp::Create, 7),
                Expect::Absent
            )
            .await,
        Err(StoreError::NotFound(_))
    ));
}

async fn resolving(store: &dyn Store) {
    let s = setup(store, "Edit resolving").await;
    let change = record(
        store,
        &update(s.item, 1, "mine", ChangeClass::Edit, 1),
        Expect::Any,
    )
    .await;
    assert_eq!(
        store
            .resolve_change(change.id, &Resolution::Retry, t(2))
            .await
            .unwrap(),
        None,
        "only a refused change is retried"
    );
    let conflict = store
        .resolve_change(
            change.id,
            &Resolution::Conflict {
                reason: "overlap".into(),
            },
            t(2),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(conflict.state, ItemChangeState::Conflict);
    assert_eq!(conflict.reason.as_deref(), Some("overlap"));
    assert_eq!(
        store
            .resolve_change(change.id, &Resolution::Rebased { onto: 1 }, t(3))
            .await
            .unwrap(),
        None
    );
    let listed = store
        .open_changes(&ItemChangeQuery {
            archive_id: s.archive,
            collection_id: Some(s.calendar),
            states: vec![ItemChangeState::Conflict],
            flagged_only: false,
            after: None,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(
        listed.iter().map(|c| c.id).collect::<Vec<_>>(),
        vec![change.id]
    );
    let discarded = store
        .resolve_change(change.id, &Resolution::Discard, t(4))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(discarded.state, ItemChangeState::Discarded);
    assert_eq!(
        discarded.content,
        content("mine"),
        "discarding keeps the bytes"
    );
    assert_eq!(
        store
            .resolve_change(change.id, &Resolution::Discard, t(5))
            .await
            .unwrap(),
        None
    );
    let now = store.effective(&item(s.item)).await.unwrap().unwrap();
    assert_eq!(now.change, None, "a discarded change is no longer served");
    assert!(now.served);
    assert!(
        store
            .open_changes(&ItemChangeQuery {
                archive_id: s.archive,
                collection_id: None,
                states: vec![],
                flagged_only: false,
                after: None,
                limit: 10,
            })
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .open_changes(&ItemChangeQuery {
                archive_id: s.archive,
                collection_id: None,
                states: vec![ItemChangeState::Discarded],
                flagged_only: false,
                after: None,
                limit: 10,
            })
            .await
            .unwrap()
            .is_empty(),
        "states that are not open match nothing"
    );

    let reply = record(
        store,
        &update(s.item, 1, "accepted", ChangeClass::Reply, 6),
        Expect::Any,
    )
    .await;
    let applied = store
        .resolve_change(reply.id, &Resolution::Applied, t(7))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(applied.state, ItemChangeState::Applied);
    assert!(matches!(
        store
            .resolve_change(ItemChangeId::new(), &Resolution::Discard, t(8))
            .await,
        Err(StoreError::NotFound(_))
    ));
}

async fn upstream_flags(store: &dyn Store) {
    let s = setup(store, "Edit flags").await;
    let change = record(
        store,
        &update(s.item, 1, "mine", ChangeClass::Edit, 1),
        Expect::Any,
    )
    .await;
    store
        .record_version(upstream(s.calendar, "/cal/a.ics", "Edit flags a v2"))
        .await
        .unwrap();
    let flagged = store.get_change(change.id).await.unwrap().unwrap();
    assert!(
        flagged.upstream_changed,
        "a newer upstream version flags the change"
    );
    let query = ItemChangeQuery {
        archive_id: s.archive,
        collection_id: None,
        states: vec![],
        flagged_only: true,
        after: None,
        limit: 10,
    };
    assert_eq!(store.open_changes(&query).await.unwrap().len(), 1);
    let after = ItemChangeQuery {
        after: Some(change.id),
        ..query.clone()
    };
    assert!(
        store.open_changes(&after).await.unwrap().is_empty(),
        "a page starts after the id it is given"
    );
    let served = store.effective(&item(s.item)).await.unwrap().unwrap();
    assert_eq!(served.state.version, Some(2));
    assert_eq!(served.state.change, Some(change.id));

    let older = store
        .resolve_change(change.id, &Resolution::Rebased { onto: 1 }, t(2))
        .await
        .unwrap()
        .unwrap();
    assert!(
        older.upstream_changed,
        "rebasing on an older version keeps the flag"
    );
    let rebased = store
        .resolve_change(change.id, &Resolution::Rebased { onto: 2 }, t(3))
        .await
        .unwrap()
        .unwrap();
    assert!(!rebased.upstream_changed);
    assert_eq!(rebased.rebased_onto, Some(2));
    assert_eq!(rebased.base_version, Some(1), "the payload never changes");
    assert!(store.open_changes(&query).await.unwrap().is_empty());
    assert!(matches!(
        store
            .resolve_change(change.id, &Resolution::Rebased { onto: 3 }, t(3))
            .await,
        Err(StoreError::Invalid(_))
    ));

    store
        .mark_items_removed(s.calendar, &["/cal/a.ics".into()], t(4))
        .await
        .unwrap();
    let deleted = store.get_change(change.id).await.unwrap().unwrap();
    assert!(deleted.upstream_deleted);
    let still = store.effective(&item(s.item)).await.unwrap().unwrap();
    assert!(
        still.served,
        "an item removed upstream stays served with an open change"
    );
    store
        .record_version(upstream(s.calendar, "/cal/a.ics", "Edit flags a v2"))
        .await
        .unwrap();
    let back = store.get_change(change.id).await.unwrap().unwrap();
    assert!(!back.upstream_deleted, "the item is back");
    assert!(back.upstream_changed);

    store
        .resolve_change(change.id, &Resolution::Rebased { onto: 2 }, t(5))
        .await
        .unwrap()
        .unwrap();
    store
        .mark_collection_removed(s.calendar, t(6))
        .await
        .unwrap();
    assert!(
        store
            .get_change(change.id)
            .await
            .unwrap()
            .unwrap()
            .upstream_deleted
    );
}

async fn writeback(store: &dyn Store) {
    let s = setup(store, "Edit writeback").await;
    let other = store
        .record_version(upstream(s.calendar, "/cal/c.ics", "Edit writeback c v1"))
        .await
        .unwrap()
        .item_id;
    let first = record(
        store,
        &update(s.item, 1, "first", ChangeClass::Edit, 1),
        Expect::Any,
    )
    .await;
    let quiet = record(
        store,
        &update(other, 1, "alarm only", ChangeClass::Personal, 2),
        Expect::Any,
    )
    .await;
    let created = record(
        store,
        &at_href(s.calendar, "b.ics", ChangeOp::Create, 3),
        Expect::Absent,
    )
    .await;

    assert!(
        store
            .due_change_collections(t(10))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.claim_change(s.calendar, t(10)).await.unwrap(),
        None,
        "a held collection sends nothing"
    );
    store
        .set_write_mode(s.calendar, WriteMode::WriteThrough)
        .await
        .unwrap();
    assert!(
        store
            .due_change_collections(t(10))
            .await
            .unwrap()
            .contains(&s.calendar)
    );

    let sending = store
        .claim_change(s.calendar, t(10))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sending.id, first.id, "first in, first out");
    assert_eq!(sending.state, ItemChangeState::Sending);
    assert_eq!(sending.attempts, 1);
    let next = record(
        store,
        &update(s.item, 1, "second", ChangeClass::Edit, 11),
        Expect::Any,
    )
    .await;
    assert_eq!(next.supersedes, Some(first.id));
    assert_eq!(
        store.get_change(first.id).await.unwrap().unwrap().state,
        ItemChangeState::Sending,
        "a change being sent is not superseded under the writer"
    );
    let create = store
        .claim_change(s.calendar, t(12))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        create.id, created.id,
        "the item's next change waits, others pass it, personal changes are never sent"
    );
    assert_eq!(store.claim_change(s.calendar, t(12)).await.unwrap(), None);
    assert_ne!(create.id, quiet.id);

    let first_done = store
        .settle_change(
            first.id,
            &Settle::Retry {
                next_attempt_at: t(100),
                error: "connection".into(),
            },
            t(13),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        first_done.state,
        ItemChangeState::Superseded,
        "a superseded change that failed is not sent again"
    );

    let written = upstream(s.calendar, "/cal/b-upstream.ics", "Edit writeback b v1");
    assert!(matches!(
        store
            .settle_change(
                create.id,
                &Settle::Applied {
                    version: Some(Box::new(upstream(CollectionId::new(), "/x.ics", "x")))
                },
                t(14)
            )
            .await,
        Err(StoreError::Invalid(_))
    ));
    let applied = store
        .settle_change(
            create.id,
            &Settle::Applied {
                version: Some(Box::new(written.clone())),
            },
            t(14),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(applied.state, ItemChangeState::Applied);
    let new_item = applied.item_id.expect("the create has its item now");
    let summary = store.get_item(new_item).await.unwrap().unwrap();
    assert_eq!(summary.item.dav_name.as_deref(), Some("b.ics"));
    assert_eq!(summary.item.remote_id, "/cal/b-upstream.ics");
    assert_eq!(summary.item.href(), "b.ics");
    let versions = store.item_versions(new_item).await.unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].origin, VersionOrigin::Writeback);
    assert_eq!(versions[0].change_id, Some(create.id));
    let by_href = store
        .effective(&ChangeTarget::Href {
            collection: s.calendar,
            href: "b.ics".into(),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_href.item.map(|i| i.item.id), Some(new_item));
    assert_eq!(by_href.change, None);
    assert!(by_href.served);
    assert_eq!(
        store.changes_of(&item(new_item)).await.unwrap()[0].id,
        create.id,
        "the create belongs to its item"
    );

    let second = store
        .claim_change(s.calendar, t(15))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.id, next.id);
    let rejected = store
        .settle_change(
            next.id,
            &Settle::Rejected {
                reason: "refused".into(),
            },
            t(16),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rejected.state, ItemChangeState::Rejected);
    assert!(
        store
            .effective(&item(s.item))
            .await
            .unwrap()
            .unwrap()
            .change
            .is_some()
    );
    assert_eq!(
        store
            .settle_change(next.id, &Settle::Applied { version: None }, t(16))
            .await
            .unwrap(),
        None,
        "only a change being sent settles"
    );
    let retried = store
        .resolve_change(next.id, &Resolution::Retry, t(17))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retried.state, ItemChangeState::Held);
    assert_eq!(retried.attempts, 0);
    store
        .claim_change(s.calendar, t(18))
        .await
        .unwrap()
        .unwrap();
    store
        .settle_change(
            next.id,
            &Settle::Retry {
                next_attempt_at: t(100),
                error: "connection".into(),
            },
            t(19),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(store.claim_change(s.calendar, t(50)).await.unwrap(), None);
    let late = store
        .claim_change(s.calendar, t(100))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(late.id, next.id);
    assert_eq!(late.last_error.as_deref(), Some("connection"));
    let released = store.release_changes(t(101)).await.unwrap();
    assert_eq!(
        released.iter().map(|c| c.id).collect::<Vec<_>>(),
        vec![next.id]
    );
    assert_eq!(released[0].state, ItemChangeState::Held);
    store
        .claim_change(s.calendar, t(102))
        .await
        .unwrap()
        .unwrap();
    let written = upstream(s.calendar, "/cal/a.ics", "Edit writeback a v2");
    assert!(matches!(
        store
            .settle_change(
                next.id,
                &Settle::Applied {
                    version: Some(Box::new(upstream(s.calendar, "/cal/c.ics", "c v9")))
                },
                t(103)
            )
            .await,
        Err(StoreError::Invalid(_))
    ));
    store
        .settle_change(
            next.id,
            &Settle::Applied {
                version: Some(Box::new(written)),
            },
            t(103),
        )
        .await
        .unwrap()
        .unwrap();
    let after = store.effective(&item(s.item)).await.unwrap().unwrap();
    assert_eq!(after.state.version, Some(2));
    assert_eq!(after.change, None);
    assert_eq!(after.latest.unwrap().change_id, Some(next.id));

    let delete = record(
        store,
        &NewItemChange {
            op: ChangeOp::Delete,
            class: ChangeClass::Delete,
            content: None,
            ..update(s.item, 2, "", ChangeClass::Edit, 104)
        },
        Expect::Any,
    )
    .await;
    assert!(
        !store
            .effective(&item(s.item))
            .await
            .unwrap()
            .unwrap()
            .served
    );
    store
        .claim_change(s.calendar, t(105))
        .await
        .unwrap()
        .unwrap();
    store
        .settle_change(delete.id, &Settle::Applied { version: None }, t(106))
        .await
        .unwrap()
        .unwrap();
    assert!(
        store
            .get_item(s.item)
            .await
            .unwrap()
            .unwrap()
            .item
            .removed_at
            .is_some(),
        "a sent delete marks the item removed; nothing is deleted"
    );

    let conflicting = record(
        store,
        &update(other, 1, "theirs moved", ChangeClass::Edit, 107),
        Expect::Any,
    )
    .await;
    assert_eq!(conflicting.supersedes, Some(quiet.id));
    store
        .claim_change(s.calendar, t(108))
        .await
        .unwrap()
        .unwrap();
    let conflict = store
        .settle_change(
            conflicting.id,
            &Settle::Conflict {
                reason: "precondition".into(),
            },
            t(109),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(conflict.state, ItemChangeState::Conflict);
    assert!(matches!(
        store
            .settle_change(
                ItemChangeId::new(),
                &Settle::Applied { version: None },
                t(110)
            )
            .await,
        Err(StoreError::NotFound(_))
    ));
}

async fn successors(store: &dyn Store) {
    let s = setup(store, "Successors").await;
    store
        .set_write_mode(s.calendar, WriteMode::WriteThrough)
        .await
        .unwrap();
    let edit = record(
        store,
        &update(s.item, 1, "edit", ChangeClass::Edit, 1),
        Expect::Any,
    )
    .await;
    let sending = store.claim_change(s.calendar, t(2)).await.unwrap().unwrap();
    assert_eq!(sending.id, edit.id);
    let delete = record(
        store,
        &NewItemChange {
            op: ChangeOp::Delete,
            class: ChangeClass::Delete,
            content: None,
            ..update(s.item, 1, "", ChangeClass::Edit, 3)
        },
        Expect::Any,
    )
    .await;
    assert_eq!(delete.supersedes, Some(edit.id));
    store
        .settle_change(
            edit.id,
            &Settle::Applied {
                version: Some(Box::new(upstream(s.calendar, "/cal/a.ics", "edit"))),
            },
            t(4),
        )
        .await
        .unwrap()
        .unwrap();
    let after = store.get_change(delete.id).await.unwrap().unwrap();
    assert!(
        !after.upstream_changed,
        "the user's own next change is not an upstream change"
    );
    assert_eq!(after.rebased_onto, Some(2), "it builds on what was written");
    let next = store
        .claim_change(s.calendar, t(5))
        .await
        .unwrap()
        .expect("the next change is sent");
    assert_eq!(next.id, delete.id);
    let conflict = store
        .settle_change(
            delete.id,
            &Settle::Conflict {
                reason: "upstream_changed".into(),
            },
            t(6),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(conflict.state, ItemChangeState::Conflict);
    assert!(!conflict.flagged(), "a settled conflict is not flagged");
}

async fn log_and_blobs(store: &dyn Store) {
    let s = setup(store, "Edit log").await;
    let before = store.changes_since(s.archive, None, 1000).await.unwrap();
    assert_eq!(before.len(), 1, "a new upstream item is logged: {before:?}");
    assert!(
        before[0].kind == ChangeKind::Item
            && before[0].item_id == Some(s.item)
            && before[0].collection_id == Some(s.calendar)
            && before[0].item_change_id.is_none()
    );
    store
        .record_version(upstream(s.calendar, "/cal/a.ics", "Edit log a v1"))
        .await
        .unwrap();
    assert_eq!(
        store.changes_since(s.archive, None, 1000).await.unwrap(),
        before,
        "the same version again is not logged"
    );
    let change = record(
        store,
        &update(
            s.item,
            1,
            "Edit log: only in a change",
            ChangeClass::Edit,
            1,
        ),
        Expect::Any,
    )
    .await;
    store
        .resolve_change(change.id, &Resolution::Discard, t(2))
        .await
        .unwrap()
        .unwrap();
    let log = store
        .changes_since(s.archive, Some(before[0].seq), 1000)
        .await
        .unwrap();
    assert_eq!(log.len(), 2, "recording and every state change are logged");
    assert!(log.windows(2).all(|w| w[0].seq < w[1].seq));
    assert!(log.iter().all(|e| e.item_change_id == Some(change.id)
        && e.item_id == Some(s.item)
        && e.collection_id == Some(s.calendar)
        && e.archive_id == s.archive));
    assert_eq!(log[1].at, t(2));
    assert!(
        store
            .changes_since(s.archive, Some(log[1].seq), 1000)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .changes_since(s.archive, Some(log[0].seq), 1000)
            .await
            .unwrap(),
        vec![log[1].clone()]
    );

    store
        .record_version(upstream(s.calendar, "/cal/a.ics", "Edit log a v2"))
        .await
        .unwrap();
    let removed_twice = vec!["/cal/a.ics".to_owned(), "/cal/a.ics".to_owned()];
    store
        .mark_items_removed(s.calendar, &removed_twice, t(3))
        .await
        .unwrap();
    store
        .record_version(upstream(s.calendar, "/cal/a.ics", "Edit log a v2"))
        .await
        .unwrap();
    let upstream_rows = store
        .changes_since(s.archive, Some(log[1].seq), 1000)
        .await
        .unwrap();
    assert_eq!(
        upstream_rows.len(),
        3,
        "a new version, one removal and the return are logged: {upstream_rows:?}"
    );
    assert!(upstream_rows.iter().all(|e| e.kind == ChangeKind::Item
        && e.item_id == Some(s.item)
        && e.item_change_id.is_none()));
    assert_eq!(upstream_rows[1].at, t(3));

    let blob = BlobId::of(b"Edit log: only in a change");
    let mut all = Vec::new();
    let mut after = None;
    loop {
        let page = store.referenced_blobs(after.as_ref(), 1000).await.unwrap();
        let done = page.len() < 1000;
        after = page.last().copied();
        all.extend(page);
        if done {
            break;
        }
    }
    assert!(
        all.contains(&blob),
        "bytes of a discarded change stay referenced"
    );
    let released = store.delete_archive(s.archive).await.unwrap();
    assert!(
        released.contains(&blob),
        "the archive's change bytes are released"
    );
    assert!(
        store
            .changes_since(s.archive, None, 1000)
            .await
            .unwrap()
            .is_empty()
    );
}

async fn schedule_tags(store: &dyn Store) {
    let s = setup(store, "schedule tags").await;
    assert_eq!(
        tag_of(store, s.item).await,
        None,
        "a new item follows its bytes"
    );
    store
        .set_schedule_tag(s.item, Some("pinned-1"))
        .await
        .unwrap();
    assert_eq!(tag_of(store, s.item).await.as_deref(), Some("pinned-1"));
    let mut merged = update(s.item, 1, "merged reply", ChangeClass::Edit, 10);
    merged.origin = ChangeOrigin::Itip;
    record(store, &merged, Expect::Any).await;
    assert_eq!(
        tag_of(store, s.item).await.as_deref(),
        Some("pinned-1"),
        "a merged reply keeps the tag"
    );
    record(
        store,
        &update(s.item, 1, "client edit", ChangeClass::Edit, 20),
        Expect::Any,
    )
    .await;
    assert_eq!(
        tag_of(store, s.item).await,
        None,
        "any other change clears the tag"
    );
    let mut pinned = update(s.item, 1, "merged again", ChangeClass::Edit, 30);
    pinned.origin = ChangeOrigin::Itip;
    pinned.schedule_tag = Some("pinned-2".into());
    record(store, &pinned, Expect::Any).await;
    assert_eq!(
        tag_of(store, s.item).await.as_deref(),
        Some("pinned-2"),
        "a merged reply pins its tag in the same step"
    );
    let mut ignored = update(s.item, 1, "client edit again", ChangeClass::Edit, 40);
    ignored.schedule_tag = Some("not-itip".into());
    record(store, &ignored, Expect::Any).await;
    assert_eq!(
        tag_of(store, s.item).await,
        None,
        "only an itip change pins"
    );
    let mut echoed = update(s.item, 1, "merged once more", ChangeClass::Edit, 50);
    echoed.origin = ChangeOrigin::Itip;
    echoed.schedule_tag = Some("pinned-3".into());
    record(store, &echoed, Expect::Any).await;
    store
        .record_version(upstream(s.calendar, "/cal/a.ics", "merged once more"))
        .await
        .unwrap();
    assert_eq!(
        tag_of(store, s.item).await.as_deref(),
        Some("pinned-3"),
        "the write-back of the merged reply keeps the tag"
    );
    store
        .record_version(upstream(
            s.calendar,
            "/cal/a.ics",
            "moved by the organizer elsewhere",
        ))
        .await
        .unwrap();
    assert_eq!(
        tag_of(store, s.item).await,
        None,
        "another upstream version clears the tag"
    );
    assert!(matches!(
        store.set_schedule_tag(ItemId::new(), Some("x")).await,
        Err(StoreError::NotFound(_))
    ));
}

async fn schedule_inbox(store: &dyn Store) {
    let archive = store.create_archive("schedule inbox").await.unwrap().id;
    let ada = store
        .create_user(NewUser {
            email: "inbox-ada@example.com".into(),
            name: "Ada".into(),
            role: InstanceRole::Owner,
            password_hash: None,
        })
        .await
        .unwrap()
        .id;
    let bob = store
        .create_user(NewUser {
            email: "inbox-bob@example.com".into(),
            name: "Bob".into(),
            role: InstanceRole::Member,
            password_hash: None,
        })
        .await
        .unwrap()
        .id;
    let first = store
        .insert_message(super::message(archive, 7101, 1_795_000_000))
        .await
        .unwrap()
        .message_id;
    let second = store
        .insert_message(super::message(archive, 7102, 1_795_000_100))
        .await
        .unwrap()
        .message_id;
    let entry = |message, part, note, at| ScheduleInboxEntry {
        archive_id: archive,
        message_id: message,
        part,
        method: "REPLY".into(),
        uid: "lunch@example.com".into(),
        note,
        received_at: t(at),
    };
    store
        .put_inbox_entry(&entry(first, 0, InboxNote::Unauthenticated, 0))
        .await
        .unwrap();
    store
        .put_inbox_entry(&entry(second, 0, InboxNote::Update, 50))
        .await
        .unwrap();
    store
        .put_inbox_entry(&entry(first, 0, InboxNote::Applied, 90))
        .await
        .unwrap();
    let listed = store.inbox_entries(archive, ada).await.unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|e| (e.message_id, e.note, e.received_at))
            .collect::<Vec<_>>(),
        [
            (second, InboxNote::Update, t(50)),
            (first, InboxNote::Applied, t(0)),
        ],
        "newest first; recording again replaces the note and keeps the time"
    );
    assert!(store.hide_inbox_entry(ada, second, 0).await.unwrap());
    assert!(store.hide_inbox_entry(ada, second, 0).await.unwrap());
    assert!(!store.hide_inbox_entry(ada, second, 3).await.unwrap());
    assert_eq!(store.inbox_entries(archive, ada).await.unwrap().len(), 1);
    assert_eq!(
        store.inbox_entries(archive, bob).await.unwrap().len(),
        2,
        "hiding is per user"
    );
    assert_eq!(store.inbox_entries_of(second).await.unwrap().len(), 1);
    store
        .delete_messages(archive, &[first], Some(ada))
        .await
        .unwrap();
    assert!(store.inbox_entries_of(first).await.unwrap().is_empty());
    assert_eq!(
        store.inbox_entries(archive, bob).await.unwrap().len(),
        1,
        "entries go with their message"
    );
}

async fn tag_of(store: &dyn Store, item: ItemId) -> Option<String> {
    store
        .get_item(item)
        .await
        .unwrap()
        .unwrap()
        .item
        .schedule_tag
}
