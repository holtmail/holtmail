//! Insight and category search in the real binary: invented newsletters from
//! `news@shop.example.com` and personal mail imported with insights on, then a domain override
//! that files the shop under `travel`. Lists follow the override at once; `category:` search
//! follows once the walk has drained the reindex queue, and a deletion review using it is
//! refused with `409` until then. Deleting the override takes the shop out of `travel` again.
//! An index whose stored schema lacks the insight fields is rebuilt at the next start. The logs
//! carry no subject, address or link of the seeded mail.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use holtmail_integration::api::{OWNER_EMAIL, OWNER_PASSWORD, encode, id};
use holtmail_integration::data::{Message, mbox};
use holtmail_integration::{Api, Holtmail, LogScanner, Options, require_it, unique};
use reqwest::Method;
use serde_json::{Value, json};

const SHOP: usize = 40;
const PERSONAL: usize = 20;
const SHOP_SENDER: &str = "news@shop.example.com";
const SHOP_SUBJECT: &str = "Example Shop weekly deals";
const PERSONAL_SUBJECT: &str = "Dinner on Friday?";
const RECIPIENT: &str = "ada@example.org";
const STILL_READING: &str = "Some messages are still being read";

struct Seeded {
    messages: Vec<Message>,
    links: Vec<String>,
    personal_senders: Vec<String>,
}

fn seeded() -> Seeded {
    let mut seeded = Seeded {
        messages: Vec::new(),
        links: Vec::new(),
        personal_senders: Vec::new(),
    };
    for i in 0..SHOP + PERSONAL {
        let token = unique("srch");
        let mut message = if i < SHOP {
            let link = format!("https://shop.example.com/leave/{token}");
            let mut message = Message::new(RECIPIENT)
                .subject(SHOP_SUBJECT)
                .header(&format!(
                    "List-Unsubscribe: <{link}>, <mailto:leave@shop.example.com?subject=leave>"
                ))
                .header("List-Unsubscribe-Post: List-Unsubscribe=One-Click")
                .header("Precedence: bulk")
                .html(&format!(
                    "<html><body><h1>Example Shop</h1><p>Invented offers on garden chairs and lamps.</p><p><a href=\"{link}\">Unsubscribe</a></p></body></html>\n"
                ));
            message.from = SHOP_SENDER.into();
            message.from_name = "Example Shop".into();
            seeded.links.push(link);
            message
        } else {
            let message = Message::new(RECIPIENT)
                .subject(PERSONAL_SUBJECT)
                .body("Hi Ada,\n\nShall we have dinner on Friday at the usual place?\n\nBo\n");
            seeded.personal_senders.push(message.from.clone());
            message
        };
        message.message_id = format!("{token}@example.org");
        seeded.messages.push(message);
    }
    seeded
}

async fn search(api: &Api, archive: &str, query: &str) -> BTreeSet<String> {
    let reply = api
        .get(&format!(
            "/api/archives/{archive}/search?q={}&limit=200",
            encode(query)
        ))
        .await
        .ok();
    let ids: BTreeSet<String> = reply["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(id)
        .collect();
    assert_eq!(reply["total"].as_u64(), Some(ids.len() as u64), "{query}");
    ids
}

async fn listed(api: &Api, archive: &str, category: &str) -> BTreeSet<String> {
    let reply = api
        .get(&format!(
            "/api/archives/{archive}/messages?category={category}&limit=500"
        ))
        .await
        .ok();
    assert!(reply["next"].is_null(), "{reply}");
    reply["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(id)
        .collect()
}

async fn status(api: &Api, archive: &str) -> Value {
    api.get(&format!("/api/archives/{archive}/insights/status"))
        .await
        .ok()
}

async fn caught_up(api: &Api, archive: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let status = status(api, archive).await;
        if status["pending"] == 0 && status["reindex"] == 0 && status["running"] == false {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "insights did not catch up: {status}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn search_until(
    api: &Api,
    archive: &str,
    query: &str,
    expected: &BTreeSet<String>,
    since: Instant,
    timeout: Duration,
) -> Duration {
    loop {
        let found = search(api, archive, query).await;
        if &found == expected {
            return since.elapsed();
        }
        assert!(
            since.elapsed() < timeout,
            "{query} found {} of {} expected messages after {timeout:?}",
            found.intersection(expected).count(),
            expected.len()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn review(api: &Api, archive: &str, query: &str) -> holtmail_integration::api::Reply {
    api.post(
        &format!("/api/archives/{archive}/deletions"),
        json!({ "query": query }),
    )
    .await
}

#[track_caller]
fn assert_refused_or_counted(reply: &holtmail_integration::api::Reply, count: usize) -> bool {
    match reply.status.as_u16() {
        409 => {
            assert!(
                reply.body["error"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(STILL_READING),
                "{}",
                reply.body
            );
            false
        }
        200 => {
            assert_eq!(
                reply.body["count"].as_u64(),
                Some(count as u64),
                "{}",
                reply.body
            );
            true
        }
        other => panic!("a deletion review answered {other}: {}", reply.body),
    }
}

fn index_meta(data: &std::path::Path) -> std::path::PathBuf {
    let mut dirs = vec![data.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("a readable directory") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.file_name().is_some_and(|n| n == "meta.json")
                && std::fs::read_to_string(&path).is_ok_and(|t| t.contains("\"categories\""))
            {
                return path;
            }
        }
    }
    panic!("no search index under {}", data.display());
}

#[tokio::test(flavor = "multi_thread")]
async fn category_search_follows_an_override() {
    require_it!();
    let seeded = seeded();
    let mut holtmail = Holtmail::start(Options {
        extra_config: "[insights]\nenabled = true\n".into(),
        ..Default::default()
    })
    .await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive("Insight search").await;
    let (_, task) = api
        .import(&archive, "shop.mbox", mbox(&seeded.messages))
        .await;
    assert_eq!(task["state"], "done", "{task}");
    caught_up(&api, &archive, Duration::from_secs(120)).await;

    let shop = search(&api, &archive, "is:newsletter").await;
    assert_eq!(shop.len(), SHOP);
    assert_eq!(search(&api, &archive, "has:unsubscribe").await, shop);
    assert_eq!(search(&api, &archive, "category:bulk").await, shop);
    let personal = search(&api, &archive, "is:personal").await;
    assert_eq!(personal.len(), PERSONAL);
    assert!(personal.is_disjoint(&shop));
    assert!(search(&api, &archive, "category:travel").await.is_empty());
    assert!(listed(&api, &archive, "travel").await.is_empty());
    assert_eq!(listed(&api, &archive, "bulk").await, shop);

    let stored = api
        .request(
            Method::PUT,
            &format!("/api/archives/{archive}/category-overrides"),
            Some(&json!({ "scope": "domain", "key": "shop.example.com", "topics": ["travel"] })),
        )
        .await
        .ok();
    let put_at = Instant::now();
    assert_eq!(stored["key"], "example.com", "{stored}");
    assert_eq!(stored["topics"], json!(["travel"]), "{stored}");
    let early = review(&api, &archive, "category:travel").await;
    let early_status = early.status.as_u16();
    assert_refused_or_counted(&early, SHOP);
    assert_eq!(
        listed(&api, &archive, "travel").await,
        shop,
        "the list did not follow the override at once"
    );
    let lag = search_until(
        &api,
        &archive,
        "category:travel",
        &shop,
        put_at,
        Duration::from_secs(60),
    )
    .await;
    eprintln!(
        "category:travel caught up {} ms after the override (first review answered {early_status})",
        lag.as_millis()
    );
    caught_up(&api, &archive, Duration::from_secs(60)).await;
    let deadline = Instant::now() + Duration::from_secs(30);
    while !assert_refused_or_counted(&review(&api, &archive, "category:travel").await, SHOP) {
        assert!(Instant::now() < deadline, "reviews stayed refused");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(search(&api, &archive, "is:newsletter").await, shop);
    assert!(
        search(&api, &archive, "category:travel is:personal")
            .await
            .is_empty()
    );
    assert_eq!(
        review(&api, &archive, "subject:dinner").await.ok()["count"].as_u64(),
        Some(PERSONAL as u64)
    );

    let removed = api
        .delete(&format!(
            "/api/archives/{archive}/category-overrides/{}",
            id(&stored)
        ))
        .await;
    let deleted_at = Instant::now();
    assert_eq!(removed.status.as_u16(), 204, "{}", removed.body);
    assert!(listed(&api, &archive, "travel").await.is_empty());
    let lag = search_until(
        &api,
        &archive,
        "category:travel",
        &BTreeSet::new(),
        deleted_at,
        Duration::from_secs(60),
    )
    .await;
    eprintln!(
        "category:travel emptied {} ms after the override was deleted",
        lag.as_millis()
    );
    assert_eq!(search(&api, &archive, "is:newsletter").await, shop);
    caught_up(&api, &archive, Duration::from_secs(60)).await;

    let rebuilt_before = holtmail.logs().matches("search index rebuilt").count();
    holtmail.stop().await;
    let meta = index_meta(&holtmail.data_dir());
    let mut written: Value =
        serde_json::from_str(&std::fs::read_to_string(&meta).expect("meta.json")).expect("JSON");
    let fields = written["schema"].as_array_mut().expect("a schema");
    let before = fields.len();
    fields.retain(|f| f["name"] != "insights" && f["name"] != "categories");
    assert_eq!(fields.len(), before - 2, "the schema had no insight fields");
    std::fs::write(&meta, written.to_string()).expect("meta.json written");

    holtmail.start_again().await;
    let api = Api::sign_in(&holtmail, OWNER_EMAIL, OWNER_PASSWORD).await;
    let deadline = Instant::now() + Duration::from_secs(120);
    while holtmail.logs().matches("search index rebuilt").count() == rebuilt_before {
        assert!(
            Instant::now() < deadline,
            "the index of the older schema was not rebuilt"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        std::fs::read_to_string(&meta)
            .expect("meta.json")
            .contains("\"categories\""),
        "the rebuilt index lacks the insight fields"
    );
    assert_eq!(search(&api, &archive, "is:newsletter").await, shop);
    assert_eq!(search(&api, &archive, "has:unsubscribe").await, shop);
    assert_eq!(search(&api, &archive, "is:personal").await, personal);
    assert!(search(&api, &archive, "category:travel").await.is_empty());
    assert_eq!(
        search(&api, &archive, "subject:dinner").await,
        personal,
        "plain search lost mail in the rebuild"
    );

    LogScanner::new()
        .secret("the shop sender", SHOP_SENDER)
        .secret("the shop host", "shop.example.com")
        .secret("the recipient", RECIPIENT)
        .secret("a subject", SHOP_SUBJECT)
        .secret("a subject", PERSONAL_SUBJECT)
        .secrets(
            "an unsubscribe link",
            seeded.links.iter().map(String::as_str),
        )
        .secrets(
            "a personal sender",
            seeded.personal_senders.iter().map(String::as_str),
        )
        .check(&holtmail.logs());
}
