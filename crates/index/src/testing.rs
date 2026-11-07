//! Conformance suite every [`SearchIndex`] implementation runs in its own tests.

use std::sync::Arc;

use holtmail_core::{Address, ArchiveId, ItemId, MessageId, ThreadId, Timestamp};

use crate::*;

/// Checks that `index` keeps the [`SearchIndex`] contract. Panics on the first violation.
///
/// `index` must be empty when the suite starts.
pub async fn conformance(index: Arc<dyn SearchIndex>) {
    messages(&*index).await;
    items(&*index).await;
    insights(&*index).await;
}

async fn messages(index: &dyn SearchIndex) {
    let archive = ArchiveId::new();
    let lunch = doc(
        archive,
        "Lunch on Friday",
        "ada@example.com",
        "Ada Lovelace",
        3,
    );
    let mut report = doc(
        archive,
        "Quarterly report",
        "bob@example.org",
        "Bob Stone",
        2,
    );
    report.body = "The numbers for the quarter are attached. Zoë checked them.".into();
    report.attachment_names = vec!["q3-report.pdf".into()];
    report.has_attachments = true;
    report.attachment_text = "Lighthouse maintenance schedule for the quarter".into();
    report.folders = vec!["Work/Reports".into()];
    report.flags = vec!["\\Seen".into(), "\\Flagged".into()];
    report.labels = vec!["Projects/2025".into(), "Важное".into()];
    let mut cafe = doc(archive, "Café opening", "carol@example.org", "Carol Ng", 1);
    cafe.body = "New café near the office. Lunch there on Friday?".into();
    let elsewhere = doc(
        ArchiveId::new(),
        "Lunch on Friday",
        "ada@example.com",
        "Ada",
        4,
    );

    index
        .put(vec![
            lunch.clone(),
            report.clone(),
            cafe.clone(),
            elsewhere.clone(),
        ])
        .await
        .unwrap();
    index.commit().await.unwrap();

    let all = search(index, archive, vec![], SortOrder::Newest).await;
    assert_eq!(
        all,
        [lunch.message_id, report.message_id, cafe.message_id],
        "not newest first"
    );

    let cases: Vec<(Vec<Clause>, Vec<MessageId>)> = vec![
        (
            vec![Clause::Text("lunch".into())],
            vec![lunch.message_id, cafe.message_id],
        ),
        (
            vec![Clause::Text("LUNCH friday".into())],
            vec![lunch.message_id, cafe.message_id],
        ),
        (vec![Clause::Text("zoe".into())], vec![report.message_id]),
        (vec![Clause::Text("cafe".into())], vec![cafe.message_id]),
        (vec![Clause::Text("quart".into())], vec![report.message_id]),
        (
            vec![Clause::Text("lun fri".into())],
            vec![lunch.message_id, cafe.message_id],
        ),
        (vec![Clause::Text("lu".into())], vec![]),
        (vec![Clause::From("car".into())], vec![cafe.message_id]),
        (
            vec![Clause::Phrase("on friday".into())],
            vec![lunch.message_id, cafe.message_id],
        ),
        (vec![Clause::Phrase("friday on".into())], vec![]),
        (vec![Clause::From("bob".into())], vec![report.message_id]),
        (
            vec![Clause::From("example.org".into())],
            vec![report.message_id, cafe.message_id],
        ),
        (
            vec![Clause::To("dana".into())],
            vec![lunch.message_id, report.message_id, cafe.message_id],
        ),
        (
            vec![Clause::Subject("report".into())],
            vec![report.message_id],
        ),
        (
            vec![Clause::Body("quarter".into())],
            vec![report.message_id],
        ),
        (vec![Clause::Filename("q3".into())], vec![report.message_id]),
        (
            vec![Clause::Text("lighthouse".into())],
            vec![report.message_id],
        ),
        (
            vec![Clause::Phrase("maintenance schedule".into())],
            vec![report.message_id],
        ),
        (vec![Clause::Body("lighthouse".into())], vec![]),
        (
            vec![Clause::Attachment("lighthouses".into())],
            vec![report.message_id],
        ),
        (vec![Clause::Attachment("numbers".into())], vec![]),
        (vec![Clause::HasAttachment], vec![report.message_id]),
        (
            vec![Clause::In("work/reports".into())],
            vec![report.message_id],
        ),
        (
            vec![Clause::In("inbox".into())],
            vec![lunch.message_id, cafe.message_id],
        ),
        (
            vec![Clause::Flag("\\Flagged".into())],
            vec![report.message_id],
        ),
        (
            vec![Clause::Label("projects/2025".into())],
            vec![report.message_id],
        ),
        (
            vec![Clause::Label("ВАЖНОЕ".into())],
            vec![report.message_id],
        ),
        (vec![Clause::Label("projects".into())], vec![]),
        (
            vec![Clause::Not(Box::new(Clause::Label("Projects/2025".into())))],
            vec![lunch.message_id, cafe.message_id],
        ),
        (
            vec![Clause::Not(Box::new(Clause::Flag("\\Seen".into())))],
            vec![lunch.message_id, cafe.message_id],
        ),
        (
            vec![Clause::Before(Timestamp::from_secs(2_000))],
            vec![cafe.message_id],
        ),
        (
            vec![Clause::After(Timestamp::from_secs(2_000))],
            vec![lunch.message_id, report.message_id],
        ),
        (
            vec![
                Clause::Text("lunch".into()),
                Clause::Not(Box::new(Clause::From("ada".into()))),
            ],
            vec![cafe.message_id],
        ),
    ];
    for (clauses, expected) in cases {
        let found = search(index, archive, clauses.clone(), SortOrder::Newest).await;
        assert_eq!(found, expected, "wrong result for {clauses:?}");
    }

    for (clauses, expected) in [
        (
            vec![Clause::Text("lighthouse".into())],
            vec![report.message_id],
        ),
        (
            vec![Clause::Text("quarter lighthouse".into())],
            vec![report.message_id],
        ),
        (
            vec![Clause::Phrase("maintenance schedule".into())],
            vec![report.message_id],
        ),
        (vec![Clause::Text("quarter".into())], vec![]),
        (vec![Clause::Text("lunch".into())], vec![]),
        (vec![Clause::HasAttachment], vec![]),
    ] {
        let results = index
            .search(
                archive,
                &SearchQuery {
                    clauses: clauses.clone(),
                },
                SortOrder::Newest,
                0,
                100,
            )
            .await
            .unwrap();
        assert_eq!(
            results.attachment_hits, expected,
            "wrong attachment hits for {clauses:?}"
        );
    }

    let relevant = search(
        index,
        archive,
        vec![Clause::Text("lunch".into())],
        SortOrder::Relevance,
    )
    .await;
    assert_eq!(relevant.len(), 2);

    let page = index
        .search(archive, &SearchQuery::default(), SortOrder::Newest, 1, 1)
        .await
        .unwrap();
    assert_eq!(
        page.hits,
        [report.message_id],
        "offset and limit are not applied"
    );
    assert_eq!(page.total, 3);

    let mut renamed = lunch.clone();
    renamed.subject = "Dinner on Saturday".into();
    index.put(vec![renamed]).await.unwrap();
    index.commit().await.unwrap();
    assert_eq!(
        search(
            index,
            archive,
            vec![Clause::Subject("lunch".into())],
            SortOrder::Newest
        )
        .await,
        Vec::<MessageId>::new(),
        "put did not replace the document"
    );
    assert_eq!(
        search(index, archive, vec![], SortOrder::Newest)
            .await
            .len(),
        3,
        "put duplicated the document"
    );

    index
        .remove(&[report.message_id, MessageId::new()])
        .await
        .unwrap();
    index.commit().await.unwrap();
    assert_eq!(
        search(index, archive, vec![], SortOrder::Newest).await,
        [lunch.message_id, cafe.message_id]
    );
}

async fn search(
    index: &dyn SearchIndex,
    archive: ArchiveId,
    clauses: Vec<Clause>,
    sort: SortOrder,
) -> Vec<MessageId> {
    index
        .search(archive, &SearchQuery { clauses }, sort, 0, 100)
        .await
        .unwrap()
        .hits
}

fn doc(
    archive: ArchiveId,
    subject: &str,
    email: &str,
    name: &str,
    kilo_secs: i64,
) -> IndexDocument {
    IndexDocument {
        message_id: MessageId::new(),
        archive_id: archive,
        thread_id: ThreadId::new(),
        subject: subject.into(),
        from: vec![Address {
            name: Some(name.into()),
            email: email.into(),
        }],
        recipients: vec![Address {
            name: Some("Dana".into()),
            email: "dana@example.net".into(),
        }],
        body: format!("{subject}. See you there."),
        attachment_names: Vec::new(),
        attachment_text: String::new(),
        folders: vec!["INBOX".into(), "inbox".into()],
        flags: Vec::new(),
        labels: Vec::new(),
        date: Timestamp::from_secs(kilo_secs * 1000),
        has_attachments: false,
        insights: Vec::new(),
        categories: Vec::new(),
    }
}

fn person(name: &str, email: &str) -> Address {
    Address {
        name: Some(name.into()),
        email: email.into(),
    }
}

fn item(archive: ArchiveId, kind: DocumentKind, title: &str, date: Option<i64>) -> ItemDocument {
    ItemDocument {
        item_id: ItemId::new(),
        archive_id: archive,
        kind,
        title: title.into(),
        organizer: Vec::new(),
        people: Vec::new(),
        text: String::new(),
        date: date.map(Timestamp::from_secs),
    }
}

async fn found_items(
    index: &dyn SearchIndex,
    archive: ArchiveId,
    clauses: Vec<Clause>,
) -> Vec<ItemId> {
    index
        .search_items(archive, &SearchQuery { clauses }, SortOrder::Newest, 0, 100)
        .await
        .unwrap()
        .hits
        .into_iter()
        .map(|h| h.item_id)
        .collect()
}

async fn items(index: &dyn SearchIndex) {
    let archive = ArchiveId::new();
    let mut mail = doc(
        archive,
        "Harbour walk notes",
        "ada@example.com",
        "Ada Quill",
        5,
    );
    mail.recipients = vec![person("Bob Stone", "bob@example.org")];
    let other_mail = doc(archive, "Invoice", "shop@example.net", "Shop", 6);
    let mut review = item(archive, DocumentKind::Event, "Lamp review", Some(2_000));
    review.organizer = vec![person("Bob Stone", "bob@example.org")];
    review.people = vec![
        person("Ada Quill", "ada@example.com"),
        person("Cleo Park", "cleo@example.org"),
    ];
    review.text = "Room 4, harbour office".into();
    let mut walk = item(archive, DocumentKind::Event, "Harbour walk", Some(3_000));
    walk.organizer = vec![person("Cleo Park", "cleo@example.org")];
    let mut ada = item(archive, DocumentKind::Contact, "Ada Quill", None);
    ada.people = vec![person("Ada Quill", "ada@example.com")];
    ada.text = "Lighthouse Society +15550100".into();
    let elsewhere = item(
        ArchiveId::new(),
        DocumentKind::Event,
        "Harbour walk",
        Some(3_000),
    );
    index
        .put(vec![mail.clone(), other_mail.clone()])
        .await
        .unwrap();
    index
        .put_items(vec![review.clone(), walk.clone(), ada.clone(), elsewhere])
        .await
        .unwrap();
    index.commit().await.unwrap();

    assert_eq!(
        search(index, archive, vec![], SortOrder::Newest).await,
        [other_mail.message_id, mail.message_id],
        "message searches never find items"
    );
    assert_eq!(
        found_items(index, archive, vec![]).await,
        [walk.item_id, review.item_id, ada.item_id],
        "events by start, the latest first, then contacts"
    );
    let total = index
        .search_items(archive, &SearchQuery::default(), SortOrder::Relevance, 1, 1)
        .await
        .unwrap();
    assert_eq!(total.total, 3);
    assert_eq!(total.hits.len(), 1);

    let mail_cases: Vec<(Vec<Clause>, Vec<MessageId>)> = vec![
        (
            vec![Clause::Kind(DocumentKind::Mail)],
            vec![other_mail.message_id, mail.message_id],
        ),
        (vec![Clause::Kind(DocumentKind::Event)], vec![]),
        (
            vec![Clause::Not(Box::new(Clause::Kind(DocumentKind::Contact)))],
            vec![other_mail.message_id, mail.message_id],
        ),
        (vec![Clause::With("ada".into())], vec![mail.message_id]),
        (
            vec![Clause::With("bob@example.org".into())],
            vec![mail.message_id],
        ),
        (vec![Clause::With("stone".into())], vec![mail.message_id]),
        (vec![Clause::With("cleo".into())], vec![]),
    ];
    for (clauses, expected) in mail_cases {
        assert_eq!(
            search(index, archive, clauses.clone(), SortOrder::Newest).await,
            expected,
            "wrong messages for {clauses:?}"
        );
    }
    let item_cases: Vec<(Vec<Clause>, Vec<ItemId>)> = vec![
        (
            vec![Clause::Text("harbour".into())],
            vec![walk.item_id, review.item_id],
        ),
        (vec![Clause::Text("lighthouse".into())], vec![ada.item_id]),
        (
            vec![Clause::Phrase("lamp review".into())],
            vec![review.item_id],
        ),
        (
            vec![Clause::Kind(DocumentKind::Event)],
            vec![walk.item_id, review.item_id],
        ),
        (vec![Clause::Kind(DocumentKind::Contact)], vec![ada.item_id]),
        (vec![Clause::Kind(DocumentKind::Mail)], vec![]),
        (
            vec![Clause::Not(Box::new(Clause::Kind(DocumentKind::Contact)))],
            vec![walk.item_id, review.item_id],
        ),
        (
            vec![Clause::With("ada".into())],
            vec![review.item_id, ada.item_id],
        ),
        (
            vec![Clause::With("cleo@example.org".into())],
            vec![walk.item_id, review.item_id],
        ),
        (
            vec![
                Clause::Kind(DocumentKind::Event),
                Clause::With("bob".into()),
            ],
            vec![review.item_id],
        ),
        (vec![Clause::From("bob".into())], vec![review.item_id]),
        (vec![Clause::To("cleo".into())], vec![review.item_id]),
        (vec![Clause::Subject("walk".into())], vec![walk.item_id]),
        (vec![Clause::Body("office".into())], vec![review.item_id]),
        (
            vec![Clause::After(Timestamp::from_secs(2_500))],
            vec![walk.item_id],
        ),
        (
            vec![Clause::Before(Timestamp::from_secs(2_500))],
            vec![review.item_id],
        ),
        (vec![Clause::In("inbox".into())], vec![]),
        (vec![Clause::HasAttachment], vec![]),
        (
            vec![Clause::Not(Box::new(Clause::Label("x".into())))],
            vec![walk.item_id, review.item_id, ada.item_id],
        ),
    ];
    for (clauses, expected) in item_cases {
        assert_eq!(
            found_items(index, archive, clauses.clone()).await,
            expected,
            "wrong items for {clauses:?}"
        );
    }
    let kinds: Vec<DocumentKind> = index
        .search_items(archive, &SearchQuery::default(), SortOrder::Newest, 0, 10)
        .await
        .unwrap()
        .hits
        .iter()
        .map(|h| h.kind)
        .collect();
    assert_eq!(
        kinds,
        [
            DocumentKind::Event,
            DocumentKind::Event,
            DocumentKind::Contact
        ]
    );

    let mut renamed = walk.clone();
    renamed.title = "Pier walk".into();
    index.put_items(vec![renamed]).await.unwrap();
    index
        .remove_items(&[ada.item_id, ItemId::new()])
        .await
        .unwrap();
    index.commit().await.unwrap();
    assert_eq!(
        found_items(index, archive, vec![Clause::Text("walk".into())]).await,
        [walk.item_id],
        "put_items replaced the document"
    );
    assert_eq!(
        found_items(index, archive, vec![]).await,
        [walk.item_id, review.item_id]
    );
    assert_eq!(
        search(index, archive, vec![], SortOrder::Newest)
            .await
            .len(),
        2,
        "removing items leaves messages"
    );
}

async fn insights(index: &dyn SearchIndex) {
    let archive = ArchiveId::new();
    let mut sign_in = doc(archive, "Your code", "login@example.com", "Login", 3);
    sign_in.insights = vec!["code".into(), "link".into()];
    sign_in.categories = vec!["kind:notification".into()];
    let mut news = doc(archive, "Weekly news", "news@example.org", "News", 2);
    news.insights = vec!["unsubscribe".into()];
    news.categories = vec!["kind:bulk".into(), "topic:travel".into()];
    let plain = doc(archive, "Hello", "ada@example.com", "Ada", 1);
    let mut elsewhere = doc(
        ArchiveId::new(),
        "Your code",
        "login@example.com",
        "Login",
        4,
    );
    elsewhere.insights = vec!["code".into()];
    elsewhere.categories = vec!["kind:bulk".into()];
    let mut contact = item(archive, DocumentKind::Contact, "code bulk", None);
    contact.text = "kind:bulk topic:travel code".into();
    index
        .put(vec![
            sign_in.clone(),
            news.clone(),
            plain.clone(),
            elsewhere.clone(),
        ])
        .await
        .unwrap();
    index.put_items(vec![contact.clone()]).await.unwrap();
    index.commit().await.unwrap();

    let not = |c: Clause| Clause::Not(Box::new(c));
    let cases: Vec<(Vec<Clause>, Vec<MessageId>)> = vec![
        (
            vec![Clause::Insight("code".into())],
            vec![sign_in.message_id],
        ),
        (
            vec![Clause::Insight("CODE".into())],
            vec![sign_in.message_id],
        ),
        (
            vec![Clause::Insight("unsubscribe".into())],
            vec![news.message_id],
        ),
        (vec![Clause::Insight("cod".into())], vec![]),
        (vec![Clause::Insight("parcel".into())], vec![]),
        (
            vec![not(Clause::Insight("code".into()))],
            vec![news.message_id, plain.message_id],
        ),
        (
            vec![Clause::Category("kind:bulk".into())],
            vec![news.message_id],
        ),
        (
            vec![Clause::Category("Topic:TRAVEL".into())],
            vec![news.message_id],
        ),
        (vec![Clause::Category("kind:bul".into())], vec![]),
        (vec![Clause::Category("bulk".into())], vec![]),
        (vec![Clause::Category("travel".into())], vec![]),
        (
            vec![not(Clause::Category("kind:bulk".into()))],
            vec![sign_in.message_id, plain.message_id],
        ),
        (
            vec![
                Clause::Insight("code".into()),
                Clause::Category("kind:notification".into()),
            ],
            vec![sign_in.message_id],
        ),
        (vec![Clause::Text("bulk".into())], vec![]),
    ];
    for (clauses, expected) in cases {
        assert_eq!(
            search(index, archive, clauses.clone(), SortOrder::Newest).await,
            expected,
            "wrong result for {clauses:?}"
        );
    }

    for clauses in [
        vec![Clause::Insight("code".into())],
        vec![Clause::Category("kind:bulk".into())],
        vec![Clause::Category("topic:travel".into())],
    ] {
        assert_eq!(
            found_items(index, archive, clauses.clone()).await,
            Vec::<ItemId>::new(),
            "items matched {clauses:?}"
        );
    }
    assert_eq!(
        found_items(index, archive, vec![not(Clause::Insight("code".into()))]).await,
        [contact.item_id],
        "an item has no insights"
    );

    let mut changed = news.clone();
    changed.insights = vec!["order".into()];
    changed.categories = vec!["kind:personal".into()];
    index.put(vec![changed]).await.unwrap();
    index.commit().await.unwrap();
    for (clauses, expected) in [
        (vec![Clause::Insight("unsubscribe".into())], vec![]),
        (vec![Clause::Category("kind:bulk".into())], vec![]),
        (vec![Clause::Category("topic:travel".into())], vec![]),
        (vec![Clause::Insight("order".into())], vec![news.message_id]),
        (
            vec![Clause::Category("kind:personal".into())],
            vec![news.message_id],
        ),
    ] {
        assert_eq!(
            search(index, archive, clauses.clone(), SortOrder::Newest).await,
            expected,
            "put kept old values for {clauses:?}"
        );
    }
}
