//! The matcher decides what the search index finds, checked on an invented corpus.

use std::collections::BTreeSet;

use holtmail_core::{AccountId, Address, ArchiveId, MessageId, ThreadId, Timestamp};
use holtmail_index::{IndexDocument, SearchIndex};
use holtmail_index_tantivy::TantivyIndex;
use holtmail_query::{AttachmentFacts, Matcher, MessageFacts, Verdict, parse};

fn person(name: &str, email: &str) -> Address {
    Address {
        name: (!name.is_empty()).then(|| name.into()),
        email: email.into(),
    }
}

fn file(name: &str, inline: bool) -> AttachmentFacts {
    AttachmentFacts {
        filename: Some(name.into()),
        content_type: "application/octet-stream".into(),
        size: 1_000,
        inline,
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).into()).collect()
}

fn message(subject: &str, from: Address, body: &str, secs: i64) -> MessageFacts {
    MessageFacts {
        message_id: MessageId::new(),
        thread_id: ThreadId::new(),
        accounts: vec![AccountId::new()],
        subject: subject.into(),
        from: vec![from],
        to: vec![person("Dana", "dana@example.net")],
        cc: Vec::new(),
        bcc: Vec::new(),
        headers: Vec::new(),
        body: Some(body.into()),
        attachments: Vec::new(),
        attachment_text: Some(String::new()),
        folders: strings(&["INBOX", "inbox"]),
        flags: Vec::new(),
        labels: Vec::new(),
        date: Timestamp::from_secs(secs),
        size: 4_096,
        insights: Some(Vec::new()),
        categories: Some(Vec::new()),
    }
}

fn corpus() -> Vec<MessageFacts> {
    let mut lunch = message(
        "Lunch on Friday",
        person("Ada Lovelace", "ada@example.com"),
        "Shall we try the new café near the harbour?",
        1_741_600_000,
    );
    lunch.flags = strings(&["\\Seen"]);

    let mut report = message(
        "Quarterly report",
        person("Bob Stone", "bob@example.org"),
        "The numbers for the quarter are attached. Repairs were finished on Monday.",
        1_743_500_000,
    );
    report.to = vec![person("", "team@example.com")];
    report.cc = vec![person("Zoë Ångström", "zoe@example.org")];
    report.attachments = vec![file("q3-report.pdf", false), file("logo.png", true)];
    report.attachment_text = Some("Lighthouse maintenance schedule for the quarter".into());
    report.folders = strings(&["Work/Reports"]);
    report.flags = strings(&["\\Seen", "\\Flagged"]);
    report.labels = strings(&["Projects/2025", "Важное"]);
    report.insights = Some(strings(&["invoice"]));
    report.categories = Some(strings(&["kind:personal", "topic:finance"]));

    let mut lease = message(
        "Договор аренды",
        person("Олег Петров", "oleg@example.org"),
        "Подписанный договор во вложении.",
        1_735_686_000,
    );
    lease.attachments = vec![file("договор.pdf", false)];
    lease.attachment_text = Some("Арендатор обязуется платить вовремя".into());
    lease.labels = strings(&["Важное"]);
    lease.categories = Some(strings(&["kind:personal"]));

    let mut order = message(
        "Your order has shipped",
        person("Shop", "noreply@shop.example.com"),
        "Track your parcel with code AB12.",
        1_746_400_000,
    );
    order.insights = Some(strings(&["order", "parcel", "link"]));
    order.categories = Some(strings(&["kind:notification", "topic:purchases"]));

    let mut newsletter = message(
        "Weekly newsletter: Straße & Søren",
        person("News", "news@example.net"),
        "Unsubscribe here. Łódź meetup on Friday.",
        1_748_700_000,
    );
    newsletter.folders = strings(&["Newsletters"]);
    newsletter.insights = Some(strings(&["unsubscribe", "link"]));
    newsletter.categories = Some(strings(&["kind:bulk"]));

    let mut reply = message(
        "Re: Lunch on Friday",
        person("Dana", "dana@example.net"),
        "Friday works. See you at the harbour office.",
        1_741_700_000,
    );
    reply.to = vec![person("Ada Lovelace", "ada@example.com")];
    reply.folders = strings(&["Sent", "sent"]);
    reply.flags = strings(&["\\Seen", "$label1"]);

    let mut bare = message("", person("", "carol@example.org"), "", 1_673_740_800);
    bare.to = vec![person("", "ada@example.com")];
    bare.bcc = vec![person("Eve Park", "eve@example.org")];
    bare.attachments = vec![file("invoice-2025.xlsx", false)];

    vec![lunch, report, lease, order, newsletter, reply, bare]
}

fn document(archive: ArchiveId, facts: &MessageFacts) -> IndexDocument {
    let files: Vec<_> = facts.attachments.iter().filter(|a| !a.inline).collect();
    IndexDocument {
        message_id: facts.message_id,
        archive_id: archive,
        thread_id: facts.thread_id,
        subject: facts.subject.clone(),
        from: facts.from.clone(),
        recipients: [&facts.to, &facts.cc, &facts.bcc]
            .into_iter()
            .flatten()
            .cloned()
            .collect(),
        body: facts.body.clone().unwrap_or_default(),
        attachment_names: files.iter().filter_map(|a| a.filename.clone()).collect(),
        attachment_text: facts.attachment_text.clone().unwrap_or_default(),
        folders: facts.folders.clone(),
        flags: facts.flags.clone(),
        labels: facts.labels.clone(),
        date: facts.date,
        has_attachments: !files.is_empty(),
        insights: facts.insights.clone().unwrap_or_default(),
        categories: facts.categories.clone().unwrap_or_default(),
    }
}

const QUERIES: &[&str] = &[
    "",
    "lunch",
    "LUNCH friday",
    "lu",
    "re",
    "quart",
    "repairing",
    "repair monday",
    "договора",
    "аренда",
    "Подпис",
    "арендатор",
    "cafe",
    "Café",
    "zoe",
    "angstrom",
    "strasse",
    "STRAẞE",
    "soren",
    "lodz",
    "lighthouse",
    "lighthouses",
    "\"maintenance schedule\"",
    "\"schedule maintenance\"",
    "\"on friday\"",
    "\"friday on\"",
    "\"lunch\"",
    "\"repairing\"",
    "\"ada lovelace\"",
    "\"!!!\"",
    "re:hello",
    "noreply",
    "example.com",
    "shop.example.com",
    "code ab12",
    "from:ada",
    "from:example.org",
    "from:\"ada lovelace\"",
    "from:\"lovelace ada\"",
    "from:\"lovelace ada@example\"",
    "from:\"!!\"",
    "from:oleg",
    "to:zoe",
    "to:eve",
    "to:ada",
    "to:example.com",
    "with:dana",
    "with:ada",
    "with:\"Bob Stone\"",
    "with:\"Stone Bob\"",
    "subject:report",
    "subject:reports",
    "subject:lunch -subject:re",
    "subject:договоры",
    "body:harbour",
    "body:lighthouse",
    "body:numbers quarter",
    "filename:q3",
    "filename:pdf",
    "filename:logo",
    "filename:invoice",
    "filename:договор",
    "attachment:lighthouses",
    "attachment:numbers",
    "attachment:арендатора",
    "attachment:\"draft copy\"",
    "has:attachment",
    "-has:attachment",
    "has:attachment budget",
    "in:inbox",
    "in:INBOX",
    "in:work/reports",
    "in:work",
    "-in:sent",
    "is:read",
    "is:unread",
    "is:starred",
    "label:projects/2025",
    "label:ВАЖНОЕ",
    "label:projects",
    "-label:важное",
    "has:invoice",
    "has:parcel -has:link",
    "has:link",
    "-has:link",
    "category:finance",
    "category:purchases",
    "is:newsletter",
    "is:personal",
    "is:notification",
    "-category:bulk",
    "before:2025",
    "after:2025-04",
    "after:2025-03-10 before:2025-03-11",
    "before:1970",
    "is:mail",
    "is:event",
    "-is:event lunch",
    "-is:contact -lunch",
    "lunch -from:ada",
    "friday -harbour",
    "harbour office",
    "nowhere",
];

#[tokio::test(flavor = "multi_thread")]
async fn the_matcher_agrees_with_the_index() {
    let corpus = corpus();
    let archive = ArchiveId::new();
    let index = TantivyIndex::in_memory().unwrap();
    index
        .put(corpus.iter().map(|f| document(archive, f)).collect())
        .await
        .unwrap();
    index.commit().await.unwrap();

    let mut found_something = 0;
    for input in QUERIES {
        let query = parse(input).unwrap();
        let indexed: BTreeSet<MessageId> = index
            .matching(archive, &query, 1_000)
            .await
            .unwrap()
            .ids
            .into_iter()
            .collect();
        let matcher = Matcher::new(query);
        let matched: BTreeSet<MessageId> = corpus
            .iter()
            .filter(|facts| {
                let verdict = matcher.test(facts);
                assert_ne!(verdict, Verdict::Unknown, "{input:?} with complete facts");
                verdict.is_match()
            })
            .map(|facts| facts.message_id)
            .collect();
        assert_eq!(matched, indexed, "matcher and index disagree on {input:?}");
        if !indexed.is_empty() && indexed.len() < corpus.len() {
            found_something += 1;
        }
    }
    assert!(
        found_something > QUERIES.len() / 2,
        "the corpus is too flat"
    );
}

#[test]
fn finds_what_the_query_language_promises() {
    let corpus = corpus();
    let subjects = |input: &str| -> Vec<&str> {
        let matcher = Matcher::parse(input).unwrap();
        corpus
            .iter()
            .filter(|f| matcher.test(f).is_match())
            .map(|f| f.subject.as_str())
            .collect()
    };
    assert_eq!(subjects("quart"), ["Quarterly report"]);
    assert_eq!(subjects("договора"), ["Договор аренды"]);
    assert_eq!(subjects("soren"), ["Weekly newsletter: Straße & Søren"]);
    assert_eq!(subjects("lighthouse"), ["Quarterly report"]);
    assert_eq!(subjects("body:lighthouse"), Vec::<&str>::new());
    assert_eq!(
        subjects("\"on friday\""),
        [
            "Lunch on Friday",
            "Weekly newsletter: Straße & Søren",
            "Re: Lunch on Friday"
        ]
    );
    assert_eq!(subjects("subject:lunch -subject:re"), ["Lunch on Friday"]);
    assert_eq!(subjects("to:eve"), [""]);
    assert_eq!(subjects("has:parcel -has:link"), Vec::<&str>::new());
    assert_eq!(subjects("filename:logo"), Vec::<&str>::new());
    assert_eq!(
        subjects("after:2025-03-10 before:2025-03-11"),
        ["Lunch on Friday"]
    );
}
