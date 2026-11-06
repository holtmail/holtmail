//! Remote images through real proxies: saving through `socks5h`, `socks5` with sign-in and an
//! HTTP proxy (squid), each URL fetched once and identical bytes stored once, the reader's
//! routes, wrong types, oversized and slow images refused, failed images fetched again, saving
//! older mail as a task that resumes after a kill, the proxy set through the API stored sealed,
//! and direct fetching of private and loopback addresses refused.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use holtmail_integration::data::{Message, now};
use holtmail_integration::images::{
    self, Images, PROXY_PASSWORD, PROXY_USER, SOCKS_AUTH_IP, SOCKS_AUTH_PORT, SOCKS_IP, SOCKS_PORT,
    SQUID_IP, SQUID_PORT, Served,
};
use holtmail_integration::services::{DOVECOT, DOVECOT_PASSWORD};
use holtmail_integration::{Api, Holtmail, LogScanner, Options, require_it};
use holtmail_sanitize::remote_key;
use reqwest::Method;
use serde_json::{Value, json};

const MESSAGES_AT_ONCE: usize = 4;

struct Instance {
    holtmail: Holtmail,
    api: Api,
    archive: String,
    user: String,
    account: Option<String>,
    seeded: Vec<Message>,
}

async fn instance(proxy: Option<&str>, save: bool) -> Instance {
    let extra_config = proxy
        .map(|p| format!("image_proxy = {p:?}\n"))
        .unwrap_or_default();
    let holtmail = Holtmail::start(Options {
        extra_config,
        ..Default::default()
    })
    .await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive("Images").await;
    if save {
        api.patch(
            &format!("/api/archives/{archive}"),
            json!({ "save_remote_images": true }),
        )
        .await
        .ok();
    }
    Instance {
        holtmail,
        api,
        archive,
        user: DOVECOT.user("images"),
        account: None,
        seeded: Vec::new(),
    }
}

impl Instance {
    async fn deliver(&mut self, bodies: &[Vec<String>]) -> Vec<Message> {
        let mut seeder = DOVECOT.seeder(&self.user).await;
        let mut out = Vec::new();
        for urls in bodies {
            let message = Message::new(&self.user).html(&images::html(urls));
            seeder.append("INBOX", &[], None, &message.bytes()).await;
            out.push(message);
        }
        seeder.logout().await;
        self.seeded.extend(out.iter().cloned());
        out
    }

    async fn sync(&mut self) {
        let synced = match &self.account {
            Some(account) => self.api.sync(&self.archive, account).await,
            None => {
                let account = self
                    .api
                    .add_imap(&self.archive, DOVECOT.account(&self.user))
                    .await;
                self.account = Some(account.clone());
                self.api.settle(&self.archive, &account).await
            }
        };
        assert_eq!(synced["status"]["error"], Value::Null, "{synced}");
    }

    async fn remote_tasks(&self) -> Vec<Value> {
        let mut tasks: Vec<Value> = self
            .api
            .tasks()
            .await
            .into_iter()
            .filter(|t| t["kind"] == "remote_images" && t["archive_id"] == self.archive.as_str())
            .collect();
        tasks.sort_by_key(|t| t["started_at"].as_i64().unwrap_or(0));
        tasks
    }

    /// Waits until a remote image task that started at or after `since` has finished and none
    /// runs, and returns the latest.
    async fn wait_remote(&self, since: i64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let tasks = self.remote_tasks().await;
            let running = tasks.iter().any(|t| t["state"] == "running");
            if !running
                && let Some(task) = tasks
                    .iter()
                    .rev()
                    .find(|t| t["started_at"].as_i64().unwrap_or(0) >= since)
            {
                return task.clone();
            }
            assert!(
                Instant::now() < deadline,
                "no image task finished: {tasks:?}"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// `(url, blob, error)` of every remote image the instance knows, by URL.
    async fn saved(&self) -> Vec<(String, String, String)> {
        self.holtmail
            .query(
                "SELECT url, blob_id, error FROM remote_images ORDER BY url",
                &[],
            )
            .await
            .into_iter()
            .map(|row| (row[0].clone(), row[1].clone(), row[2].clone()))
            .collect()
    }

    async fn image(&self, url: &str) -> (String, String) {
        self.saved()
            .await
            .into_iter()
            .find(|(u, _, _)| u == url)
            .map(|(_, blob, error)| (blob, error))
            .unwrap_or_else(|| panic!("{url} is not known"))
    }

    async fn totals(&self) -> Value {
        self.api
            .get(&format!("/api/archives/{}", self.archive))
            .await
            .ok()["remote_images"]
            .clone()
    }

    async fn message_id(&self, message: &Message) -> String {
        self.api
            .messages(&self.archive)
            .await
            .into_iter()
            .find(|m| m["subject"] == message.subject.as_str())
            .map(|m| holtmail_integration::api::id(&m))
            .expect("the message is archived")
    }

    fn scanner(&self) -> LogScanner {
        LogScanner::new()
            .secret("the IMAP password", DOVECOT_PASSWORD)
            .secret("the user", &self.user)
            .secret("the proxy password", PROXY_PASSWORD)
            .secret("an image server address", images::IMAGES_IP)
            .secret("an image server name", images::IMAGES_NAME)
            .secrets("a subject", self.seeded.iter().map(|m| m.subject.as_str()))
            .secrets("a sender", self.seeded.iter().map(|m| m.from.as_str()))
    }
}

async fn raw_get(api: &Api, path: &str) -> (u16, Vec<u8>) {
    let (status, bytes) = api.get_bytes(path).await;
    (status.as_u16(), bytes)
}

#[tokio::test(flavor = "multi_thread")]
async fn saves_through_socks5h_each_url_once_and_equal_bytes_once() {
    require_it!();
    let files = Images::new("dedup");
    let seed = images::seed(&holtmail_integration::unique("seed"));
    let png = images::png(seed);
    let jpeg = images::jpeg(seed);
    let a = files.put(Served::Plain, "a.png", &png);
    let b = files.put(Served::Plain, "b.jpg", &jpeg);
    let c = files.put(Served::Plain, "c.png", &png);
    for path in [&a, &b, &c] {
        files.wait_served(path).await;
    }
    let (ua, ub, uc) = (
        Images::by_name(&a),
        Images::by_name(&b),
        Images::by_name(&c),
    );

    let mut it = instance(Some(&format!("socks5h://127.0.0.1:{SOCKS_PORT}")), true).await;
    let since = now().timestamp_millis();
    let delivered = it
        .deliver(&[vec![ua.clone(), ub.clone(), uc.clone()], vec![ua.clone()]])
        .await;
    it.sync().await;
    let task = it.wait_remote(since).await;
    assert_eq!(task["state"], "done", "{task}");

    let saved = it.saved().await;
    assert_eq!(saved.len(), 3, "{saved:?}");
    assert!(
        saved
            .iter()
            .all(|(_, blob, error)| !blob.is_empty() && error.is_empty()),
        "{saved:?}"
    );
    let (blob_a, _) = it.image(&ua).await;
    let (blob_b, _) = it.image(&ub).await;
    let (blob_c, _) = it.image(&uc).await;
    assert_eq!(
        blob_a, blob_c,
        "equal bytes behind two URLs were stored twice"
    );
    assert_ne!(blob_a, blob_b);
    let blobs: BTreeSet<_> = it
        .holtmail
        .query("SELECT DISTINCT blob_id FROM remote_images", &[])
        .await
        .into_iter()
        .map(|row| row[0].clone())
        .collect();
    assert_eq!(blobs.len(), 2);
    let totals = it.totals().await;
    assert_eq!(totals["images"], 3, "{totals}");
    assert_eq!(totals["failed"], 0, "{totals}");
    assert_eq!(totals["older_pending"], 0, "{totals}");
    let linked = it
        .holtmail
        .query("SELECT COUNT(*) FROM message_remote_images", &[])
        .await;
    assert_eq!(linked[0][0], "4");

    for path in [&a, &b, &c] {
        assert_eq!(
            images::wait_requests(path, 1).await,
            vec![SOCKS_IP.to_owned()],
            "{path} was not fetched once through the proxy"
        );
    }

    let first = it.message_id(&delivered[0]).await;
    let (status, bytes) = raw_get(
        &it.api,
        &format!("/api/messages/{first}/remote/{}", remote_key(&ub)),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(bytes, jpeg);
    let (status, bytes) = raw_get(
        &it.api,
        &format!("/api/messages/{first}/remote/{}", remote_key(&uc)),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(bytes, png);
    assert_eq!(
        images::requests(&b).len(),
        1,
        "a saved copy was fetched again"
    );
    let (status, bytes) = raw_get(
        &it.api,
        &format!("/api/messages/{first}/live/{}", remote_key(&ub)),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(bytes, jpeg);
    assert_eq!(
        images::wait_requests(&b, 2).await,
        vec![SOCKS_IP.to_owned(), SOCKS_IP.to_owned()],
        "the live route did not go through the proxy"
    );
    let second = it.message_id(&delivered[1]).await;
    let (status, _) = raw_get(
        &it.api,
        &format!("/api/messages/{second}/remote/{}", remote_key(&ub)),
    )
    .await;
    assert_eq!(status, 404, "a message served an image it does not link");

    it.scanner().check(&it.holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_wrong_types_oversized_and_slow_images_and_retries_failed_ones() {
    require_it!();
    let files = Images::new("refused");
    let seed = images::seed(&holtmail_integration::unique("seed"));
    let good = files.put(Served::Plain, "good.png", &images::png(seed));
    let wrong = files.put(
        Served::Wrong,
        "page.png",
        b"<html><body>Not an image</body></html>\n",
    );
    let big = files.put(
        Served::Plain,
        "big.png",
        &images::oversized(11 * 1024 * 1024),
    );
    let slow = files.put(
        Served::Slow,
        "slow.png",
        &images::noise_png(64 * 1024, seed),
    );
    let later = files.path(Served::Plain, "later.png");
    for path in [&good, &wrong, &big, &slow] {
        files.wait_served(path).await;
    }
    let urls: Vec<String> = [&good, &wrong, &big, &slow, &later]
        .iter()
        .map(|p| Images::by_name(p))
        .collect();

    let mut it = instance(Some(&format!("socks5h://127.0.0.1:{SOCKS_PORT}")), true).await;
    let since = now().timestamp_millis();
    it.deliver(std::slice::from_ref(&urls)).await;
    it.sync().await;
    let started = Instant::now();
    let task = it.wait_remote(since).await;
    assert_eq!(task["state"], "done", "{task}");
    assert!(
        task["log"]
            .to_string()
            .contains("4 images could not be fetched"),
        "{task}"
    );

    let expect = [
        (&urls[0], ""),
        (&urls[1], "not an image"),
        (&urls[2], "too large"),
        (&urls[3], "timed out"),
        (&urls[4], "HTTP 404"),
    ];
    for (url, reason) in expect {
        let (blob, error) = it.image(url).await;
        if reason.is_empty() {
            assert!(!blob.is_empty() && error.is_empty(), "{url}: {error}");
        } else {
            assert!(blob.is_empty(), "{url} was saved");
            assert!(
                error.contains(reason),
                "{url}: {error:?}, expected {reason:?}"
            );
        }
    }
    let totals = it.totals().await;
    assert_eq!(
        (totals["images"].as_u64(), totals["failed"].as_u64()),
        (Some(1), Some(4)),
        "{totals}"
    );
    eprintln!("first pass with a slow image took {:?}", started.elapsed());

    files.put(Served::Plain, "later.png", &images::jpeg(seed ^ 1));
    files.wait_served(&later).await;
    let since = now().timestamp_millis();
    let started = it
        .api
        .post(
            &format!("/api/archives/{}/remote-images", it.archive),
            json!({ "job": "failed" }),
        )
        .await;
    assert_eq!(started.status, 202, "{}", started.body);
    let task = it.wait_remote(since).await;
    assert_eq!(task["state"], "done", "{task}");
    let (blob, error) = it.image(&urls[4]).await;
    assert!(
        !blob.is_empty() && error.is_empty(),
        "the image that came back: {error}"
    );
    for url in &urls[1..4] {
        assert!(it.image(url).await.0.is_empty(), "{url} was saved on retry");
    }
    let totals = it.totals().await;
    assert_eq!(
        (totals["images"].as_u64(), totals["failed"].as_u64()),
        (Some(2), Some(3)),
        "{totals}"
    );
    assert_eq!(
        images::requests(&good).len(),
        1,
        "a saved image was fetched again on retry"
    );

    it.scanner().check(&it.holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn saves_through_an_http_proxy() {
    require_it!();
    let files = Images::new("squid");
    let seed = images::seed(&holtmail_integration::unique("seed"));
    let png = images::png(seed);
    let path = files.put(Served::Plain, "squid.png", &png);
    files.wait_served(&path).await;
    let before = images::requests(&path).len();
    let url = Images::by_address(&path);

    let mut it = instance(Some(&format!("http://127.0.0.1:{SQUID_PORT}")), true).await;
    let status = it.api.get("/api/image-proxy").await.ok();
    assert_eq!(status["from_config"], true, "{status}");
    let since = now().timestamp_millis();
    let delivered = it.deliver(&[vec![url.clone()]]).await;
    it.sync().await;
    it.wait_remote(since).await;
    let (blob, error) = it.image(&url).await;
    assert!(!blob.is_empty(), "{error}");
    let seen = images::wait_requests(&path, before + 1).await;
    assert_eq!(seen[before..], [SQUID_IP.to_owned()], "{seen:?}");
    let message = it.message_id(&delivered[0]).await;
    let (status, bytes) = raw_get(
        &it.api,
        &format!("/api/messages/{message}/remote/{}", remote_key(&url)),
    )
    .await;
    assert_eq!((status, bytes == png), (200, true));
    let refused = it
        .api
        .request(
            Method::PUT,
            "/api/image-proxy",
            Some(&json!({ "proxy": null })),
        )
        .await;
    assert_eq!(
        refused.status, 400,
        "a proxy from the config was changed: {}",
        refused.body
    );

    it.scanner().check(&it.holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_socks5_proxy_with_a_password_set_through_the_api_is_stored_sealed() {
    require_it!();
    let files = Images::new("socks-auth");
    let seed = images::seed(&holtmail_integration::unique("seed"));
    let first = files.put(Served::Plain, "first.png", &images::png(seed));
    let second = files.put(Served::Plain, "second.png", &images::png(seed ^ 7));
    for path in [&first, &second] {
        files.wait_served(path).await;
    }
    let (u1, u2) = (Images::by_address(&first), Images::by_address(&second));

    let mut it = instance(None, true).await;
    let wrong = format!("socks5://{PROXY_USER}:not-the-password@127.0.0.1:{SOCKS_AUTH_PORT}");
    it.api
        .request(
            Method::PUT,
            "/api/image-proxy",
            Some(&json!({ "proxy": wrong })),
        )
        .await
        .ok();
    let since = now().timestamp_millis();
    it.deliver(&[vec![u1.clone()]]).await;
    it.sync().await;
    it.wait_remote(since).await;
    let (blob, error) = it.image(&u1).await;
    assert!(
        blob.is_empty(),
        "fetched through a proxy that refused the password"
    );
    assert!(!error.is_empty());
    assert!(
        images::requests(&first).is_empty(),
        "the image server was reached"
    );

    let proxy = format!("socks5://{PROXY_USER}:{PROXY_PASSWORD}@127.0.0.1:{SOCKS_AUTH_PORT}");
    let status = it
        .api
        .request(
            Method::PUT,
            "/api/image-proxy",
            Some(&json!({ "proxy": proxy })),
        )
        .await
        .ok();
    assert_eq!(
        status["address"],
        format!("socks5://127.0.0.1:{SOCKS_AUTH_PORT}").as_str(),
        "{status}"
    );
    assert!(!status.to_string().contains(PROXY_PASSWORD), "{status}");
    let stored = it
        .holtmail
        .query("SELECT value FROM settings WHERE key = 'image_proxy'", &[])
        .await;
    assert_eq!(stored.len(), 1);
    for clear in [PROXY_PASSWORD, PROXY_USER, "socks5://", "127.0.0.1"] {
        assert!(
            !stored[0][0].contains(clear),
            "the stored proxy holds {clear} in the clear"
        );
    }
    let database = std::fs::read(it.holtmail.database()).expect("the database");
    assert!(
        !database
            .windows(PROXY_PASSWORD.len())
            .any(|w| w == PROXY_PASSWORD.as_bytes()),
        "the proxy password is in the database file"
    );

    let since = now().timestamp_millis();
    it.api
        .post(
            &format!("/api/archives/{}/remote-images", it.archive),
            json!({ "job": "failed" }),
        )
        .await
        .ok();
    it.wait_remote(since).await;
    assert!(
        !it.image(&u1).await.0.is_empty(),
        "the retry through the right proxy failed"
    );
    assert_eq!(
        images::wait_requests(&first, 1).await,
        vec![SOCKS_AUTH_IP.to_owned()]
    );

    it.holtmail.restart().await;
    it.api = Api::sign_in(
        &it.holtmail,
        holtmail_integration::api::OWNER_EMAIL,
        holtmail_integration::api::OWNER_PASSWORD,
    )
    .await;
    let status = it.api.get("/api/image-proxy").await.ok();
    assert_eq!(
        status["address"],
        format!("socks5://127.0.0.1:{SOCKS_AUTH_PORT}").as_str(),
        "{status}"
    );
    let since = now().timestamp_millis();
    it.deliver(&[vec![u2.clone()]]).await;
    it.sync().await;
    it.wait_remote(since).await;
    assert!(
        !it.image(&u2).await.0.is_empty(),
        "not saved after a restart"
    );
    assert_eq!(
        images::wait_requests(&second, 1).await,
        vec![SOCKS_AUTH_IP.to_owned()]
    );

    it.scanner()
        .secret("the proxy URL", &proxy)
        .check(&it.holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn saving_older_mail_resumes_after_a_kill() {
    require_it!();
    const COUNT: usize = 16;
    let files = Images::new("older");
    let seed = images::seed(&holtmail_integration::unique("seed"));
    let paths: Vec<String> = (0..COUNT)
        .map(|i| {
            files.put(
                Served::Paced,
                &format!("{i}.png"),
                &images::noise_png(24 * 1024, seed.wrapping_add(i as u32)),
            )
        })
        .collect();
    files.wait_served(&paths[COUNT - 1]).await;

    let mut it = instance(Some(&format!("socks5h://127.0.0.1:{SOCKS_PORT}")), false).await;
    let bodies: Vec<Vec<String>> = paths.iter().map(|p| vec![Images::by_name(p)]).collect();
    it.deliver(&bodies).await;
    it.sync().await;
    assert!(
        it.saved().await.is_empty(),
        "images saved while saving was off"
    );

    it.api
        .patch(
            &format!("/api/archives/{}", it.archive),
            json!({ "save_remote_images": true }),
        )
        .await
        .ok();
    let totals = it.totals().await;
    assert_eq!(totals["older_pending"], COUNT, "{totals}");
    let started = it
        .api
        .post(
            &format!("/api/archives/{}/remote-images", it.archive),
            json!({ "job": "older" }),
        )
        .await;
    assert_eq!(started.status, 202, "{}", started.body);
    let task = holtmail_integration::api::id(&started.body);

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let listed = it.remote_tasks().await;
        let current = listed
            .iter()
            .find(|t| t["id"] == task.as_str())
            .expect("listed");
        let done = current["done"].as_u64().unwrap_or(0);
        assert_eq!(
            current["state"], "running",
            "finished before it could be stopped: {current}"
        );
        if done >= 4 {
            break;
        }
        assert!(Instant::now() < deadline, "no progress: {current}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    it.holtmail.kill();
    let scanned = it
        .holtmail
        .query("SELECT COUNT(*) FROM remote_scans", &[])
        .await[0][0]
        .parse::<usize>()
        .unwrap();
    assert!(
        (4..COUNT).contains(&scanned),
        "{scanned} of {COUNT} looked at before the kill"
    );

    let since = now().timestamp_millis();
    it.holtmail.start_again().await;
    it.api = Api::sign_in(
        &it.holtmail,
        holtmail_integration::api::OWNER_EMAIL,
        holtmail_integration::api::OWNER_PASSWORD,
    )
    .await;
    let resumed = it.wait_remote(since).await;
    assert_eq!(resumed["state"], "done", "{resumed}");
    let tasks = it.remote_tasks().await;
    let killed = tasks
        .iter()
        .find(|t| t["id"] == task.as_str())
        .expect("still listed");
    assert_eq!(killed["state"], "interrupted", "{killed}");
    let totals = it.totals().await;
    assert_eq!(totals["older_pending"], 0, "{totals}");
    assert_eq!(totals["images"], COUNT, "{totals}");
    assert_eq!(totals["failed"], 0, "{totals}");
    let mut fetched_twice = 0;
    for path in &paths {
        let seen = images::requests(path);
        assert!(!seen.is_empty(), "{path} was never fetched");
        fetched_twice += usize::from(seen.len() > 1);
    }
    assert!(
        fetched_twice <= MESSAGES_AT_ONCE,
        "{fetched_twice} images fetched again after the restart; only those in flight should be"
    );

    it.scanner().check(&it.holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_sender_does_not_hold_up_the_other_messages() {
    require_it!();
    let files = Images::new("unblocked");
    let seed = images::seed(&holtmail_integration::unique("seed"));
    let slow = files.put(
        Served::Slow,
        "slow.png",
        &images::noise_png(20 * 1024, seed),
    );
    let fast: Vec<String> = (0..6)
        .map(|i| {
            files.put(
                Served::Plain,
                &format!("{i}.png"),
                &images::png(seed.wrapping_add(i + 1)),
            )
        })
        .collect();
    files.wait_served(&slow).await;
    for path in &fast {
        files.wait_served(path).await;
    }
    let slow_url = Images::by_name(&slow);
    let fast_urls: Vec<String> = fast.iter().map(|p| Images::by_name(p)).collect();

    let mut it = instance(Some(&format!("socks5h://127.0.0.1:{SOCKS_PORT}")), true).await;
    let since = now().timestamp_millis();
    let mut bodies = vec![vec![slow_url.clone()]];
    bodies.extend(fast_urls.iter().map(|u| vec![u.clone()]));
    it.deliver(&bodies).await;
    let started = Instant::now();
    it.sync().await;

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let saved = it.saved().await;
        let fast_saved = fast_urls
            .iter()
            .filter(|u| {
                saved
                    .iter()
                    .any(|(url, blob, _)| url == *u && !blob.is_empty())
            })
            .count();
        if fast_saved == fast_urls.len() {
            assert!(
                !saved.iter().any(|(url, _, _)| url == &slow_url),
                "the slow image finished first, so nothing waited for it"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the other images were not saved: {saved:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let others_after = started.elapsed();
    let task = it.wait_remote(since).await;
    assert_eq!(task["state"], "done", "{task}");
    let (blob, error) = it.image(&slow_url).await;
    assert!(
        !blob.is_empty() && error.is_empty(),
        "the slow image: {error}"
    );
    eprintln!(
        "six fast images saved {:.1} s after the sync started, the slow one after {:.1} s",
        others_after.as_secs_f64(),
        started.elapsed().as_secs_f64()
    );
    it.scanner().check(&it.holtmail.logs());
}

#[tokio::test(flavor = "multi_thread")]
async fn direct_fetching_refuses_private_and_loopback_addresses() {
    require_it!();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let connections = Arc::new(AtomicUsize::new(0));
    let counted = connections.clone();
    let server = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            use tokio::io::AsyncWriteExt;
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: image/gif\r\nContent-Length: 14\r\n\r\nGIF89a\x01\x00\x01\x00\x00\x00\x00;")
                .await;
        }
    });
    let refused = [
        format!("http://127.0.0.1:{port}/a.gif"),
        format!("http://localhost:{port}/b.gif"),
        format!("http://[::1]:{port}/c.gif"),
        format!("http://[::ffff:127.0.0.1]:{port}/d.gif"),
        format!("http://0.0.0.0:{port}/e.gif"),
        "http://10.1.2.3/f.gif".to_owned(),
        "http://192.168.1.1/g.gif".to_owned(),
        "http://172.16.0.1/h.gif".to_owned(),
        "http://169.254.169.254/latest/i.gif".to_owned(),
        "http://100.64.0.1/j.gif".to_owned(),
        "http://holtmail-it.internal/k.gif".to_owned(),
    ];
    let unknown = "http://images.it.test/plain/l.gif".to_owned();

    let mut it = instance(None, true).await;
    let mut urls = refused.to_vec();
    urls.push(unknown.clone());
    let since = now().timestamp_millis();
    let delivered = it.deliver(&[urls]).await;
    it.sync().await;
    it.wait_remote(since).await;
    for url in &refused {
        let (blob, error) = it.image(url).await;
        assert!(blob.is_empty(), "{url} was saved");
        assert!(error.contains("not a public address"), "{url}: {error}");
    }
    let (blob, error) = it.image(&unknown).await;
    assert!(blob.is_empty() && !error.is_empty(), "{unknown}: {error}");

    let message = it.message_id(&delivered[0]).await;
    for url in &refused[..5] {
        for route in ["remote", "live"] {
            let (status, _) = raw_get(
                &it.api,
                &format!("/api/messages/{message}/{route}/{}", remote_key(url)),
            )
            .await;
            assert_eq!(status, 404, "{route} {url}");
        }
    }
    let since = now().timestamp_millis();
    it.api
        .post(
            &format!("/api/archives/{}/remote-images", it.archive),
            json!({ "job": "failed" }),
        )
        .await
        .ok();
    it.wait_remote(since).await;
    assert_eq!(it.totals().await["failed"], refused.len() + 1);
    assert_eq!(
        connections.load(Ordering::SeqCst),
        0,
        "Holtmail connected to a loopback address"
    );
    server.abort();

    it.scanner().check(&it.holtmail.logs());
}
