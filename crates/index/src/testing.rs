//! Conformance suite every [`SearchIndex`] implementation runs in its own tests.

use std::sync::Arc;

use holtmail_core::{Address, ArchiveId, MessageId, ThreadId, Timestamp};

use crate::*;

/// Checks that `index` keeps the [`SearchIndex`] contract. Panics on the first violation.
///
/// `index` must be empty when the suite starts.
pub async fn conformance(index: Arc<dyn SearchIndex>) {
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

    let all = search(&*index, archive, vec![], SortOrder::Newest).await;
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
        let found = search(&*index, archive, clauses.clone(), SortOrder::Newest).await;
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
        &*index,
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
            &*index,
            archive,
            vec![Clause::Subject("lunch".into())],
            SortOrder::Newest
        )
        .await,
        Vec::<MessageId>::new(),
        "put did not replace the document"
    );
    assert_eq!(
        search(&*index, archive, vec![], SortOrder::Newest)
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
        search(&*index, archive, vec![], SortOrder::Newest).await,
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
    }
}
