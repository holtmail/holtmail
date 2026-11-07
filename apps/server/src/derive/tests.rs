use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use bytes::Bytes;
use holtmail_api::Mailboxes;
use holtmail_blob::{BlobError, BlobId, BlobStore};
use holtmail_blob_fs::FsBlobStore;
use holtmail_category::testing::FakeCategorizer;
use holtmail_category::{Categorizer, CategorySource, CategoryVote, Topic, TopicSet, TrustState};
use holtmail_category_data::{DomainTable, Header, Row};
use holtmail_category_domains::DomainsCategorizer;
use holtmail_category_signals::SignalsCategorizer;
use holtmail_core::{AccountId, MessageId, Timestamp, UserId};
use holtmail_index_tantivy::TantivyIndex;
use holtmail_ingest::{Ingest, Placement};
use holtmail_insights::DeriveConfig;
use holtmail_store::{
    AccountKind, ArchiveStore, CategoryOverride, CategoryOverrideId, CategoryRow, CategoryStore,
    InsightStore, MessageStore, NewAccount, OverrideScope,
};
use holtmail_store_sqlite::SqliteStore;

use super::*;
use crate::insights::InsightsConfig;

struct Counting {
    inner: Arc<dyn BlobStore>,
    gets: AtomicU64,
}

impl Counting {
    fn take(&self) -> u64 {
        self.gets.swap(0, Ordering::SeqCst)
    }
}

#[async_trait]
impl BlobStore for Counting {
    async fn put(&self, bytes: Bytes) -> Result<BlobId, BlobError> {
        self.inner.put(bytes).await
    }

    async fn get(&self, id: &BlobId) -> Result<Option<Bytes>, BlobError> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        self.inner.get(id).await
    }

    async fn contains(&self, id: &BlobId) -> Result<bool, BlobError> {
        self.inner.contains(id).await
    }

    async fn delete(&self, id: &BlobId) -> Result<(), BlobError> {
        self.inner.delete(id).await
    }
}

struct Fixture {
    store: Arc<SqliteStore>,
    blobs: Arc<Counting>,
    index: Arc<TantivyIndex>,
    _dir: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
    let fs: Arc<dyn BlobStore> =
        Arc::new(FsBlobStore::open(dir.path().join("blobs")).await.unwrap());
    let index = Arc::new(TantivyIndex::open(dir.path().join("index")).unwrap());
    Fixture {
        store,
        blobs: Arc::new(Counting {
            inner: fs,
            gets: AtomicU64::new(0),
        }),
        index,
        _dir: dir,
    }
}

fn mail(n: usize, domain: &str) -> Bytes {
    Bytes::from(format!(
        "From: Shop <no-reply@{domain}>\r\nTo: ada@example.org\r\nSubject: Your code {n}\r\n\
         Message-ID: <derive{n}@{domain}>\r\nDate: Mon, 10 Mar 2025 09:00:00 +0000\r\n\
         List-Unsubscribe: <https://{domain}/u/{n}>\r\n\r\n\
         Your verification code is 4829{n:02}.\r\n"
    ))
}

impl Fixture {
    fn insights_only(&self) -> Ingest {
        Ingest::new(self.blobs.clone(), self.store.clone(), self.index.clone())
            .with_insights(Arc::new(InsightsConfig::default().runner()))
    }

    fn categorized(&self, deriver: &Arc<Deriver>) -> Ingest {
        self.insights_only()
            .with_categories(deriver.clone(), deriver.policy())
    }

    async fn archive(&self, name: &str) -> ArchiveId {
        self.store.create_archive(name).await.unwrap().id
    }

    async fn add(
        &self,
        ingest: &Ingest,
        archive: ArchiveId,
        range: std::ops::Range<usize>,
        domain: &str,
    ) -> Vec<MessageId> {
        let mut ids = Vec::new();
        for n in range {
            ids.push(
                ingest
                    .ingest(archive, mail(n, domain), &Placement::default())
                    .await
                    .unwrap()
                    .message_id,
            );
        }
        ingest.commit().await.unwrap();
        ids
    }

    async fn row(&self, id: MessageId) -> Option<CategoryRow> {
        self.store
            .insight_view(id)
            .await
            .unwrap()
            .and_then(|view| view.categories)
            .map(|view| view.row)
    }

    async fn fp(&self, deriver: &Deriver, archive: ArchiveId) -> String {
        deriver
            .context(&*self.store, archive)
            .await
            .unwrap()
            .derive_fp()
            .to_owned()
    }

    async fn stale(&self, deriver: &Deriver, archive: ArchiveId) -> u64 {
        let fp = self.fp(deriver, archive).await;
        self.store.count_needing_derive(archive, &fp).await.unwrap()
    }
}

fn signals() -> Arc<Deriver> {
    let mut config = InsightsConfig::default();
    config.categories.sources = vec!["signals".into()];
    Arc::new(config.deriver(&[], None))
}

#[tokio::test(flavor = "multi_thread")]
async fn derives_every_archive_without_reading_a_blob() {
    let f = fixture().await;
    let deriver = signals();
    let (a, b) = (f.archive("A").await, f.archive("B").await);
    let ingest = f.insights_only();
    let ids = f.add(&ingest, a, 0..5, "shop.example.com").await;
    f.add(&ingest, b, 5..8, "news.example.org").await;
    assert_eq!(f.row(ids[0]).await, None);
    assert_eq!(f.stale(&deriver, a).await, 5);
    f.blobs.take();

    let derived = derive_if_needed(&*f.store, &deriver, deriver.policy())
        .await
        .unwrap();
    assert_eq!((derived.rows, derived.restarts, derived.failed), (8, 0, 0));
    assert_eq!(derived.batches, 2);
    assert_eq!(f.blobs.take(), 0, "the stage-B pass read a blob");
    assert_eq!(
        (f.stale(&deriver, a).await, f.stale(&deriver, b).await),
        (0, 0)
    );
    let row = f.row(ids[0]).await.unwrap();
    assert_eq!(row.write.derive_fp, f.fp(&deriver, a).await);
    assert_eq!(row.write.sender_domain.as_deref(), Some("example.com"));
    assert_eq!(row.write.trust, TrustState::Unknown);

    let again = derive_if_needed(&*f.store, &deriver, deriver.policy())
        .await
        .unwrap();
    assert_eq!(again, Derived::default());

    let shop = CategoryOverride {
        id: CategoryOverrideId::new(),
        archive_id: a,
        scope: OverrideScope::Domain,
        key: "example.com".into(),
        kind: None,
        topics: Some(TopicSet::from(Topic::Jobs)),
        created_at: Timestamp::from_secs(10),
        created_by: UserId::new(),
    };
    let (stored, changed) = f
        .store
        .put_category_override(&shop, deriver.policy())
        .await
        .unwrap();
    assert_eq!(changed.len(), 5);
    assert!(f.row(ids[3]).await.unwrap().topics.contains(Topic::Jobs));
    f.store
        .delete_category_override(a, stored.id, deriver.policy())
        .await
        .unwrap();
    assert_eq!(f.blobs.take(), 0, "an override edit read a blob");
    assert_eq!(f.stale(&deriver, a).await, 0, "an override made rows stale");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_walk_reads_each_message_once_and_the_pass_none() {
    let f = fixture().await;
    let deriver = signals();
    let archive = f.archive("Mail").await;
    let plain = Ingest::new(f.blobs.clone(), f.store.clone(), f.index.clone());
    let ids = f.add(&plain, archive, 0..6, "shop.example.com").await;
    f.blobs.take();

    let ingest = f.categorized(&deriver);
    let runner = Arc::new(InsightsConfig::default().runner());
    let walk = Arc::new(
        crate::walk::Walk::new(
            f.store.clone(),
            f.blobs.clone(),
            ingest.clone(),
            vec![Arc::new(crate::walk::InsightsStep::new(
                ingest,
                runner.extract_fp(),
                f.store.clone(),
            ))],
            Arc::new(crate::tasks::Tasks::default()),
        )
        .with_derive(deriver.clone(), deriver.policy()),
    );
    let ran = walk.run().await.unwrap();
    assert_eq!(ran.messages, 6);
    assert_eq!(f.blobs.take(), 6, "one read per message");
    let derived = derive_if_needed(&*f.store, &deriver, deriver.policy())
        .await
        .unwrap();
    assert_eq!(derived.rows, 0, "the walk's stage B left rows behind");
    assert_eq!(f.blobs.take(), 0);
    for id in ids {
        assert_eq!(
            f.row(id).await.unwrap().write.derive_fp,
            f.fp(&deriver, archive).await
        );
    }
}

fn vote(topic: Topic) -> HashMap<String, CategoryVote> {
    HashMap::from([(
        "example.com".to_owned(),
        CategoryVote {
            kind: None,
            kind_source: CategorySource::Domain,
            topics: TopicSet::from(topic),
            source: CategorySource::Domain,
        },
    )])
}

fn with_fake(fake: Arc<FakeCategorizer>) -> Deriver {
    Deriver::new(
        vec![
            Arc::new(SignalsCategorizer::new()) as Arc<dyn Categorizer>,
            fake,
        ],
        DeriveConfig {
            sources: vec!["signals".into(), "fake".into()],
            ..DeriveConfig::default()
        },
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn restarts_with_fresh_snapshots_after_a_swap_and_converges() {
    let f = fixture().await;
    let fake = Arc::new(FakeCategorizer::new("fake", "1", vote(Topic::Jobs)));
    let deriver = with_fake(fake.clone());
    let fp_new = {
        let swapped = Arc::new(FakeCategorizer::new("fake", "2", vote(Topic::Social)));
        let archive = f.archive("Mail").await;
        let old = f.fp(&deriver, archive).await;
        let new = f.fp(&with_fake(swapped), archive).await;
        assert_ne!(old, new);
        (archive, old, new)
    };
    let (archive, fp_old, fp_new) = fp_new;
    let ids = f
        .add(&f.insights_only(), archive, 0..12, "shop.example.com")
        .await;
    f.blobs.take();

    let store = f.store.clone();
    let check_ids = ids.clone();
    let (old, new) = (fp_old.clone(), fp_new.clone());
    let checks = AtomicU64::new(0);
    let after_batch = |batch: u64| {
        let store = store.clone();
        let ids = check_ids.clone();
        let (old, new) = (old.clone(), new.clone());
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                for id in ids {
                    let Some(view) = store.insight_view(id).await.unwrap() else {
                        continue;
                    };
                    let Some(categories) = view.categories else {
                        continue;
                    };
                    let row = categories.row;
                    let topics = row.write.derived_topics;
                    if row.write.derive_fp == old {
                        assert!(topics.contains(Topic::Jobs) && !topics.contains(Topic::Social));
                    } else {
                        assert_eq!(row.write.derive_fp, new, "a row of an unknown context");
                        assert!(topics.contains(Topic::Social) && !topics.contains(Topic::Jobs));
                    }
                }
            })
        });
        checks.fetch_add(1, Ordering::SeqCst);
        if batch == 1 {
            fake.swap("2", vote(Topic::Social));
        }
    };
    let derived = derive_with(
        &*f.store,
        &deriver,
        deriver.policy(),
        Limits { page: 5, batch: 2 },
        &after_batch,
    )
    .await
    .unwrap();
    assert_eq!(derived.restarts, 1);
    assert_eq!(derived.rows, 12 + 2, "the first batch is written again");
    assert_eq!(derived.failed, 0);
    assert_eq!(checks.load(Ordering::SeqCst), derived.batches);
    assert_eq!(f.blobs.take(), 0);
    for id in &ids {
        let row = f.row(*id).await.unwrap();
        assert_eq!(row.write.derive_fp, fp_new);
        assert!(row.topics.contains(Topic::Social));
    }
    assert_eq!(
        f.store
            .count_needing_derive(archive, &fp_new)
            .await
            .unwrap(),
        0
    );
    let again = derive_if_needed(&*f.store, &deriver, deriver.policy())
        .await
        .unwrap();
    assert_eq!(again.rows, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn queues_changed_categories_and_the_walk_indexes_them_after_the_pass() {
    use holtmail_index::{Clause, SearchIndex, SearchQuery, SortOrder};

    let f = fixture().await;
    let fake = Arc::new(FakeCategorizer::new("fake", "1", vote(Topic::Jobs)));
    let deriver = Arc::new(with_fake(fake.clone()));
    let archive = f.archive("Mail").await;
    let ids = f
        .add(&f.insights_only(), archive, 0..4, "shop.example.com")
        .await;
    let queued = || async { f.store.reindex_queue(archive, None, 100).await.unwrap() };

    let derived = derive_if_needed(&*f.store, &deriver, deriver.policy())
        .await
        .unwrap();
    assert_eq!(derived.changed, 4);
    let first = queued().await;
    assert_eq!(first.iter().map(|(id, _)| *id).collect::<Vec<_>>(), ids);

    fake.swap("2", vote(Topic::Jobs));
    let same = derive_if_needed(&*f.store, &deriver, deriver.policy())
        .await
        .unwrap();
    assert_eq!((same.rows, same.changed), (4, 0));
    assert_eq!(queued().await, first, "an unchanged row was queued again");

    let ingest = f.categorized(&deriver);
    let walk = Arc::new(
        crate::walk::Walk::new(
            f.store.clone(),
            f.blobs.clone(),
            ingest.clone(),
            vec![
                Arc::new(crate::walk::InsightsStep::new(
                    ingest.clone(),
                    InsightsConfig::default().runner().extract_fp(),
                    f.store.clone(),
                )),
                Arc::new(crate::walk::IndexStep::new(ingest, f.store.clone())),
            ],
            Arc::new(crate::tasks::Tasks::default()),
        )
        .with_derive(deriver.clone(), deriver.policy()),
    );
    fake.swap("3", vote(Topic::Social));
    f.blobs.take();
    let walking = tokio::spawn(crate::walk::run(walk, Arc::default()));
    let search = |topic: &'static str| {
        let index = f.index.clone();
        async move {
            index
                .search(
                    archive,
                    &SearchQuery {
                        clauses: vec![Clause::Category(format!("topic:{topic}"))],
                    },
                    SortOrder::Newest,
                    0,
                    10,
                )
                .await
                .unwrap()
                .total
        }
    };
    let mut drained = false;
    for _ in 0..200 {
        if queued().await.is_empty() && search("social").await == 4 {
            drained = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    walking.abort();
    assert!(drained, "the walk did not index what the pass queued");
    assert_eq!(search("jobs").await, 0);
    assert_eq!(f.stale(&deriver, archive).await, 0);
    assert_eq!(
        f.blobs.take(),
        8,
        "one read per message for each of two walk runs"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn redoes_archives_finished_before_a_swap() {
    let f = fixture().await;
    let fake = Arc::new(FakeCategorizer::new("fake", "1", vote(Topic::Jobs)));
    let deriver = with_fake(fake.clone());
    let (a, b) = (f.archive("A").await, f.archive("B").await);
    let ingest = f.insights_only();
    let first = f.add(&ingest, a, 0..3, "shop.example.com").await;
    f.add(&ingest, b, 3..6, "shop.example.com").await;

    let derived = derive_with(
        &*f.store,
        &deriver,
        deriver.policy(),
        Limits {
            page: 10,
            batch: 10,
        },
        &|batch| {
            if batch == 2 {
                fake.swap("2", vote(Topic::Social));
            }
        },
    )
    .await
    .unwrap();
    assert_eq!(derived.restarts, 2, "{derived:?}");
    assert_eq!(derived.rows, 3 + 3 + 3 + 3);
    assert_eq!(
        (f.stale(&deriver, a).await, f.stale(&deriver, b).await),
        (0, 0)
    );
    assert!(
        f.row(first[0])
            .await
            .unwrap()
            .topics
            .contains(Topic::Social)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_trust_list_change_rederives_that_archive_only() {
    let f = fixture().await;
    let deriver = signals();
    let mut accounts: Vec<AccountId> = Vec::new();
    let mut archives = Vec::new();
    let ingest = f.categorized(&deriver);
    for (n, name) in ["A", "B"].into_iter().enumerate() {
        let archive = f.archive(name).await;
        accounts.push(
            f.store
                .create_account(NewAccount {
                    archive_id: archive,
                    kind: AccountKind::Imap,
                    name: format!("Mail {name}"),
                    address: Some(format!("ada{n}@example.org")),
                    settings: serde_json::json!({}),
                    sealed_secret: None,
                    enabled: false,
                })
                .await
                .unwrap()
                .id,
        );
        f.add(&ingest, archive, n * 3..n * 3 + 3, "shop.example.com")
            .await;
        archives.push(archive);
    }
    let derived = derive_if_needed(&*f.store, &deriver, deriver.policy())
        .await
        .unwrap();
    assert_eq!(derived.rows, 0, "ingest left rows behind");
    let before: Vec<String> = [
        f.fp(&deriver, archives[0]).await,
        f.fp(&deriver, archives[1]).await,
    ]
    .into();

    f.store
        .set_trusted_authserv_ids(accounts[0], &["mx.example.org".into()])
        .await
        .unwrap();
    let after = [
        f.fp(&deriver, archives[0]).await,
        f.fp(&deriver, archives[1]).await,
    ];
    assert_ne!(before[0], after[0]);
    assert_eq!(before[1], after[1]);
    assert_eq!(f.stale(&deriver, archives[0]).await, 3);
    assert_eq!(f.stale(&deriver, archives[1]).await, 0);
    f.blobs.take();

    let derived = derive_if_needed(&*f.store, &deriver, deriver.policy())
        .await
        .unwrap();
    assert_eq!(derived.rows, 3);
    assert_eq!(f.blobs.take(), 0);
    assert_eq!(f.stale(&deriver, archives[0]).await, 0);
}

const SIGNED_FORWARD: &[u8] =
    b"Authentication-Results: mx.example.org; dkim=pass header.d=sl.example.org\r\n\
    X-SimpleLogin-Type: Forward\r\nX-SimpleLogin-Envelope-To: shop@alias.example.org\r\n\
    From: \"Grace Hopper - grace at example.com\" <ra_q1@sl.example.org>\r\n\
    To: shop@alias.example.org\r\nSubject: Parcel\r\nMessage-ID: <p2@example.com>\r\n\
    Date: Fri, 14 Mar 2025 11:00:00 +0000\r\n\r\nOn its way.\r\n";

#[tokio::test(flavor = "multi_thread")]
async fn alias_changes_rederive_without_blob_reads() {
    let f = fixture().await;
    let deriver = signals();
    let archive = f.archive("Mail").await;
    let ingest = f.categorized(&deriver);
    let id = ingest
        .ingest(
            archive,
            Bytes::from_static(SIGNED_FORWARD),
            &Placement::default(),
        )
        .await
        .unwrap()
        .message_id;
    f.add(&ingest, archive, 0..3, "shop.example.com").await;
    assert_eq!(f.row(id).await.unwrap().write.trust, TrustState::Unknown);
    assert_eq!(f.blobs.take(), 0);

    let detecting = ingest.with_alias_detectors(vec![Arc::new(
        holtmail_alias_simplelogin::SimpleLoginDetector::new(&["sl.example.org".into()]),
    )]);
    let (changed, archives) = crate::aliases::detect_all(&*f.store, &detecting, "test")
        .await
        .unwrap();
    assert_eq!((changed, archives), (1, vec![archive]));
    assert_eq!(f.blobs.take(), 4, "detection reads each blob once");
    assert_eq!(
        f.row(id).await,
        None,
        "the alias change left the stage-B row"
    );
    assert_eq!(
        f.stale(&deriver, archive).await,
        4,
        "the alias address joins the own addresses"
    );

    let derived = derive_if_needed(&*f.store, &deriver, deriver.policy())
        .await
        .unwrap();
    assert_eq!(derived.rows, 4);
    assert_eq!(f.blobs.take(), 0, "stage B read a blob");
    let row = f.row(id).await.unwrap();
    assert_eq!(
        row.write.trust,
        TrustState::Relayed {
            provider: "simplelogin".into()
        }
    );
    assert_eq!(row.write.sender_domain.as_deref(), Some("example.com"));
}

#[tokio::test(flavor = "multi_thread")]
async fn insights_changed_wakes_the_pass() {
    let dir = tempfile::tempdir().unwrap();
    let config: crate::config::Config =
        toml::from_str(&format!("data_dir = {:?}", dir.path())).unwrap();
    let services = Arc::new(crate::config::Services::open(&config).await.unwrap());
    let schedule = Arc::new(
        crate::schedule::Schedule::open(None, services.store.clone())
            .await
            .unwrap(),
    );
    let runner =
        crate::mailboxes::Runner::open(services.clone(), dir.path().join("uploads"), schedule)
            .await
            .unwrap();
    let store = services.store.clone();
    let insights = services.insights.as_ref().unwrap();
    let deriver = insights.deriver.clone();
    let archive = store.create_archive("Mail").await.unwrap().id;
    let ingest = services.ingest();
    ingest
        .ingest(archive, mail(1, "shop.example.com"), &Placement::default())
        .await
        .unwrap();
    ingest.commit().await.unwrap();

    let (walk, wake) = crate::walk(&services, &runner).unwrap();
    assert!(Arc::ptr_eq(&wake, &insights.wake));
    let walking = tokio::spawn(crate::walk::run(walk, wake));
    let stale = || async {
        let fp = deriver
            .context(&*store, archive)
            .await
            .unwrap()
            .derive_fp()
            .to_owned();
        store.count_needing_derive(archive, &fp).await.unwrap()
    };
    let settle = || async {
        for _ in 0..200 {
            if stale().await == 0 {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        false
    };
    assert!(settle().await, "the start-up pass did not run");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    store
        .create_account(NewAccount {
            archive_id: archive,
            kind: AccountKind::Import,
            name: "Old mail".into(),
            address: None,
            settings: serde_json::json!({}),
            sealed_secret: None,
            enabled: false,
        })
        .await
        .unwrap();
    assert_eq!(stale().await, 1);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(stale().await, 1, "the pass ran without a wake");

    runner.insights_changed(archive);
    assert!(settle().await, "insights_changed did not wake the pass");
    walking.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn times_the_pass_on_generated_rows() {
    const ROWS: u32 = if cfg!(debug_assertions) {
        2_000
    } else {
        20_000
    };
    let f = fixture().await;
    let deriver = signals();
    let archive = f.archive("Generated").await;
    let id = f
        .add(&f.insights_only(), archive, 0..1, "shop.example.com")
        .await[0];
    let template = f.store.insight_view(id).await.unwrap().unwrap().insights;
    for n in 0..ROWS {
        let message = f
            .store
            .insert_message(holtmail_store::testing::message(archive, n, i64::from(n)))
            .await
            .unwrap()
            .message_id;
        let mut row = template.clone();
        row.message = message;
        f.store.set_message_insights(message, &row).await.unwrap();
    }
    let started = std::time::Instant::now();
    let derived = derive_if_needed(&*f.store, &deriver, deriver.policy())
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(derived.rows, u64::from(ROWS) + 1);
    println!(
        "stage B over {} rows: {elapsed:?}, {:.0} rows/s, slowest batch of {BATCH} {:?}",
        derived.rows,
        derived.rows as f64 / elapsed.as_secs_f64(),
        derived.slowest_batch
    );
}

fn table(name: &str, version: &str, rows: &[(&str, Topic)]) -> DomainTable {
    let mut rows: Vec<Row> = rows
        .iter()
        .map(|(domain, topic)| Row {
            domain: (*domain).to_owned(),
            topics: TopicSet::from(*topic),
            source: format!("test:{domain}"),
        })
        .collect();
    rows.sort_by(|a, b| a.domain.cmp(&b.domain));
    let gz = holtmail_category_data::write(&Header::new(name, version), &rows).unwrap();
    holtmail_category_data::parse(&gz).unwrap()
}

fn with_domains(domains: &Arc<DomainsCategorizer>) -> Arc<Deriver> {
    Arc::new(InsightsConfig::default().deriver(&[], Some(domains.clone())))
}

fn bank(n: usize, signed: bool) -> Bytes {
    let signature = if signed {
        "DKIM-Signature: v=1; a=rsa-sha256; d=example.org; s=s1; h=from:subject; bh=x; b=ZmFrZWZhSignedForReal\r\n"
    } else {
        ""
    };
    Bytes::from(format!(
        "Authentication-Results: mx.example.org; dkim=pass header.d=example.org header.s=s1 header.b=ZmFrZWZh\r\n\
         {signature}From: Example Bank <billing@example.org>\r\nTo: ada@example.com\r\n\
         Subject: Your statement {n}\r\nMessage-ID: <bank{n}@example.org>\r\n\
         Date: Fri, 06 Nov 2026 08:00:00 +0000\r\n\r\nYour statement is ready.\r\n"
    ))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_domain_table_votes_topics_and_sensitive_ones_need_aligned_trust() {
    let f = fixture().await;
    let domains = Arc::new(
        DomainsCategorizer::new(vec![table(
            "wikidata",
            "1",
            &[
                ("shop.example.com", Topic::Purchases),
                ("example.org", Topic::Finance),
            ],
        )])
        .unwrap(),
    );
    let deriver = with_domains(&domains);
    let archive = f.archive("Mail").await;
    let account = f
        .store
        .create_account(NewAccount {
            archive_id: archive,
            kind: AccountKind::Imap,
            name: "Mail".into(),
            address: Some("ada@example.com".into()),
            settings: serde_json::json!({}),
            sealed_secret: None,
            enabled: false,
        })
        .await
        .unwrap()
        .id;
    f.store
        .set_trusted_authserv_ids(account, &["mx.example.org".into()])
        .await
        .unwrap();
    let folder = f
        .store
        .upsert_folder(account, "INBOX", None)
        .await
        .unwrap()
        .id;
    let ingest = f.insights_only();
    let shop = f.add(&ingest, archive, 0..2, "shop.example.com").await;
    let mut bank_ids = Vec::new();
    for (uid, signed) in [(1, true), (2, false)] {
        let placement = Placement {
            folder: Some(folder),
            uid: Some(uid),
            ..Placement::default()
        };
        bank_ids.push(
            ingest
                .ingest(archive, bank(uid as usize, signed), &placement)
                .await
                .unwrap()
                .message_id,
        );
    }
    ingest.commit().await.unwrap();
    f.blobs.take();

    let derived = derive_if_needed(&*f.store, &deriver, deriver.policy())
        .await
        .unwrap();
    assert_eq!((derived.rows, derived.failed), (4, 0));
    assert_eq!(f.blobs.take(), 0, "the stage-B pass read a blob");
    assert_eq!(f.stale(&deriver, archive).await, 0);

    let row = f.row(shop[0]).await.unwrap();
    assert_eq!(row.write.trust, TrustState::Unknown);
    assert!(row.topics.contains(Topic::Purchases));
    assert_eq!(
        row.write.derived_sources.get(&Topic::Purchases),
        Some(&CategorySource::Domain)
    );

    let aligned = f.row(bank_ids[0]).await.unwrap();
    assert_eq!(
        aligned.write.trust,
        TrustState::Aligned {
            domain: "example.org".into()
        }
    );
    assert!(aligned.topics.contains(Topic::Finance));
    assert_eq!(
        aligned.write.derived_sources.get(&Topic::Finance),
        Some(&CategorySource::Domain)
    );

    let spoofed = f.row(bank_ids[1]).await.unwrap();
    assert_eq!(spoofed.write.trust, TrustState::Unverified);
    assert!(!spoofed.write.derived_topics.contains(Topic::Finance));
    assert!(!spoofed.topics.contains(Topic::Finance));
    assert!(spoofed.write.derived_sources.is_empty());

    let unlisted = signals();
    assert_ne!(
        f.fp(&unlisted, archive).await,
        f.fp(&deriver, archive).await
    );
    let derived = derive_if_needed(&*f.store, &unlisted, unlisted.policy())
        .await
        .unwrap();
    assert_eq!(derived.rows, 4);
    assert_eq!(f.blobs.take(), 0);
    assert!(
        !f.row(shop[0])
            .await
            .unwrap()
            .topics
            .contains(Topic::Purchases)
    );
    assert!(
        !f.row(bank_ids[0])
            .await
            .unwrap()
            .topics
            .contains(Topic::Finance)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_domain_reload_mid_pass_restarts_and_converges() {
    let f = fixture().await;
    let domains = Arc::new(
        DomainsCategorizer::new(vec![table(
            "wikidata",
            "1",
            &[("shop.example.com", Topic::Purchases)],
        )])
        .unwrap(),
    );
    let deriver = with_domains(&domains);
    let archive = f.archive("Mail").await;
    let fp_old = f.fp(&deriver, archive).await;
    let next = table("wikidata", "2", &[("shop.example.com", Topic::Travel)]);
    let fp_new = {
        let swapped = Arc::new(DomainsCategorizer::new(vec![next.clone()]).unwrap());
        f.fp(&with_domains(&swapped), archive).await
    };
    assert_ne!(fp_old, fp_new);
    let ids = f
        .add(&f.insights_only(), archive, 0..12, "shop.example.com")
        .await;
    f.blobs.take();

    let store = f.store.clone();
    let checks = AtomicU64::new(0);
    let after_batch = |batch: u64| {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                for id in &ids {
                    let Some(categories) = store
                        .insight_view(*id)
                        .await
                        .unwrap()
                        .and_then(|view| view.categories)
                    else {
                        continue;
                    };
                    let write = categories.row.write;
                    let topics = write.derived_topics;
                    if write.derive_fp == fp_old {
                        assert!(
                            topics.contains(Topic::Purchases) && !topics.contains(Topic::Travel)
                        );
                    } else {
                        assert_eq!(write.derive_fp, fp_new, "a row of an unknown context");
                        assert!(
                            topics.contains(Topic::Travel) && !topics.contains(Topic::Purchases)
                        );
                    }
                }
            })
        });
        checks.fetch_add(1, Ordering::SeqCst);
        if batch == 1 {
            domains.reload(vec![next.clone()]).unwrap();
        }
    };
    let derived = derive_with(
        &*f.store,
        &deriver,
        deriver.policy(),
        Limits { page: 5, batch: 2 },
        &after_batch,
    )
    .await
    .unwrap();
    assert_eq!(derived.restarts, 1);
    assert_eq!(derived.rows, 12 + 2, "the first batch is written again");
    assert_eq!(derived.failed, 0);
    assert_eq!(checks.load(Ordering::SeqCst), derived.batches);
    assert_eq!(f.blobs.take(), 0, "the stage-B pass read a blob");
    for id in &ids {
        let row = f.row(*id).await.unwrap();
        assert_eq!(row.write.derive_fp, fp_new);
        assert!(row.topics.contains(Topic::Travel));
        assert!(!row.topics.contains(Topic::Purchases));
        assert_eq!(
            row.write.derived_sources.get(&Topic::Travel),
            Some(&CategorySource::Domain)
        );
    }
    assert_eq!(f.stale(&deriver, archive).await, 0);
    let again = derive_if_needed(&*f.store, &deriver, deriver.policy())
        .await
        .unwrap();
    assert_eq!(again.rows, 0);
}
