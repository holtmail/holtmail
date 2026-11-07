//! Category data updates from a container (`IT_PROFILES=docker`): Holtmail runs from
//! `holtmail:it-insights` (`FEATURES=insights-test-data`, which compiles in the test key and the
//! test-only `update_*` keys), trusts the stack's CA and checks every 2 s for a release at
//! `https://data.it.test/<run>/`, pinned to `data.it.test` and `mirror.it.test`. The test builds
//! and signs the releases itself. A first release newer than the bundled table is fetched,
//! verified, written to `/data/categories/`, shown by `GET /api/insights/data` as downloaded and
//! re-derives an archived message's topic with no new import; the next check is a `304` and the
//! data file is not fetched again. A manifest signed by another key, an expired one and an older
//! version are refused and the loaded version stays. After a restart a release reached through a
//! `302` to the second pinned name loads with UT1 and its attribution; a `302` to a name that is
//! not pinned is refused without a request. The logs carry no row, hash, sender or key.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use holtmail_category::Topic;
use holtmail_integration::api::{OWNER_EMAIL, OWNER_PASSWORD, id};
use holtmail_integration::categories::{
    DATA_NAME, DATA_NETWORK, Fetch, MIRROR_NAME, Release, TEST_KEY_ID, fetches, stranger_key,
    table, test_key,
};
use holtmail_integration::docker::{Container, INSIGHTS_IMAGE, Volume, image_present};
use holtmail_integration::unsubscribe::{OUTSIDE_NAME, UNLISTED_NETWORK, requests};
use holtmail_integration::{Api, LogScanner, profile, require_it, state_dir, unique};
use serde_json::Value;

const WIKIDATA: &str = "wikidata-categories.tsv.gz";
const UT1: &str = "ut1-categories.tsv.gz";
const SHOP: &str = "shop.example.com";
const JOBS: &str = "jobs.example.org";
const FIRST: &str = "2099010100";
const SECOND: &str = "2099020100";
const OLDER: &str = "2099000100";
const THIRD: &str = "2099030100";
const FOURTH: &str = "2099040100";
const DAY: i64 = 24 * 60 * 60;

fn config(path: &Path, base: &str, sources: &[&str]) {
    let sources = sources
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        path,
        format!(
            "tls_ca_file = \"/certs/ca.crt\"\n\n[insights.categories]\nupdate = true\ndata_sources = [{sources}]\nupdate_base_url = \"{base}\"\nupdate_hosts = [\"{DATA_NAME}\", \"{MIRROR_NAME}\"]\nupdate_interval_secs = 2\nupdate_delay_secs = 1\n"
        ),
    )
    .expect("config written");
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_secs() as i64
}

fn mbox(token: &str) -> Vec<u8> {
    let mut out = String::new();
    for (from, subject) in [
        (format!("news@{SHOP}"), "Spring sale"),
        (format!("hiring@{JOBS}"), "Open roles"),
    ] {
        out.push_str(&format!("From {from} Sat Nov  7 09:00:00 2026\n"));
        out.push_str(&format!(
            "From: Example Sender <{from}>\n\
             To: <reader@example.org>\n\
             Subject: {subject} {token}\n\
             Date: Sat, 07 Nov 2026 09:00:00 +0000\n\
             Message-ID: <{subject_id}-{token}@example.net>\n\
             MIME-Version: 1.0\n\
             Content-Type: text/plain; charset=utf-8\n\
             \n\
             The garden club meets on Thursday.\n",
            subject_id = subject.replace(' ', "-").to_lowercase(),
        ));
    }
    out.into_bytes()
}

async fn owner(container: &Container) -> Api {
    Api::sign_in_url(&container.url, OWNER_EMAIL, OWNER_PASSWORD).await
}

async fn data(api: &Api) -> Value {
    api.get("/api/insights/data").await.ok()
}

fn file<'a>(data: &'a Value, name: &str) -> &'a Value {
    data["files"]
        .as_array()
        .and_then(|files| files.iter().find(|f| f["name"] == name))
        .unwrap_or(&Value::Null)
}

async fn wait_data(api: &Api, what: &str, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let data = data(api).await;
        if done(&data) {
            return data;
        }
        assert!(Instant::now() < deadline, "{what}: {data}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_topics(api: &Api, message: &str, what: &str, done: impl Fn(&Value) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let insights = api.message(message).await["insights"].clone();
        if !insights["kind"].is_null() && done(&insights["topics"]) {
            return;
        }
        assert!(Instant::now() < deadline, "{what}: {insights}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn has_topic(topics: &Value, topic: &str) -> bool {
    topics.as_array().is_some_and(|t| {
        t.iter()
            .any(|t| t["id"] == topic && t["source"] == "domain")
    })
}

fn refusals(container: &Container) -> Vec<String> {
    container
        .logs()
        .lines()
        .filter(|l| l.contains("category data refused"))
        .map(str::to_owned)
        .collect()
}

/// Waits until a refusal after the first `since` names `reason`, and returns how many there are.
async fn wait_refused(container: &Container, since: usize, reason: &str) -> usize {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let seen = refusals(container);
        if seen.len() > since
            && seen.last().is_some_and(|l| {
                l.contains(&format!("reason={reason}"))
                    || l.contains(&format!("reason=\"{reason}\""))
            })
        {
            return seen.len();
        }
        assert!(
            Instant::now() < deadline,
            "no refusal for {reason}:\n{}",
            seen[since.min(seen.len())..].join("\n")
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn of(fetches: &[Fetch], host: &str, path: &str) -> Vec<Fetch> {
    fetches
        .iter()
        .filter(|f| f.host == host && f.uri == path)
        .cloned()
        .collect()
}

async fn wait_fetches(prefix: &str, what: &str, done: impl Fn(&[Fetch]) -> bool) -> Vec<Fetch> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let seen = fetches(prefix);
        if done(&seen) {
            return seen;
        }
        assert!(Instant::now() < deadline, "{what}: {seen:#?}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn stored(container: &Container, into: &Path) -> PathBuf {
    let _ = std::fs::remove_dir_all(into);
    container.copy_out("/data/categories", into);
    into.to_path_buf()
}

#[tokio::test(flavor = "multi_thread")]
async fn signed_releases_load_and_bad_ones_are_refused() {
    require_it!();
    if !profile("docker") {
        eprintln!("skipped: needs IT_PROFILES=docker");
        return;
    }
    assert!(
        image_present(INSIGHTS_IMAGE),
        "it.sh builds {INSIGHTS_IMAGE}"
    );
    let run = unique("data");
    let dir = state_dir().join("category-data").join(&run);
    std::fs::create_dir_all(&dir).expect("config directory");
    let release_dir = state_dir().join("data").join(&run);
    std::fs::create_dir_all(&release_dir).expect("release directory");
    let config_file = dir.join("holtmail.toml");
    config(
        &config_file,
        &format!("https://{DATA_NAME}/{run}/"),
        &["wikidata"],
    );
    let certs = format!("{}:/certs:ro", state_dir().display());
    let inside = format!("/certs/category-data/{run}/holtmail.toml");
    let volume = Volume::new("holtmail-it-data");
    let mut container = Container::run(
        INSIGHTS_IMAGE,
        &volume,
        &[("HOLTMAIL_SYNC_INTERVAL", "0")],
        &["-v", &certs],
        &["--config", &inside, "serve"],
    )
    .await;
    container.connect(DATA_NETWORK);
    container.connect(UNLISTED_NETWORK);
    let added = container.exec(
        &[
            "user",
            "add",
            "--email",
            OWNER_EMAIL,
            "--name",
            "Owner Example",
        ],
        &format!("{OWNER_PASSWORD}\n"),
    );
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let api = owner(&container).await;
    let archive = api.create_archive("Shops").await;
    let token = unique("sale");
    let (_, task) = api.import(&archive, "shops.mbox", mbox(&token)).await;
    assert_eq!(task["state"], "done", "{task}");
    let messages = api.messages(&archive).await;
    let find = |word: &str| {
        messages
            .iter()
            .find(|m| m["subject"].as_str().is_some_and(|s| s.starts_with(word)))
            .map(id)
            .unwrap_or_else(|| panic!("{word} was not imported"))
    };
    let shop = find("Spring sale");
    let jobs = find("Open roles");
    wait_topics(&api, &shop, "stage B on the shop", |_| true).await;
    wait_topics(&api, &jobs, "stage B on the jobs", |_| true).await;
    let topics = api.message(&shop).await["insights"]["topics"].clone();
    assert!(!has_topic(&topics, "purchases"), "{topics}");

    let before = wait_data(&api, "a failed first check", |d| {
        d["updates"]["last_outcome"] == "failed"
    })
    .await;
    assert_eq!(before["updates"]["enabled"], true, "{before}");
    assert_eq!(
        before["updates"]["data_sources"],
        serde_json::json!(["wikidata"]),
        "{before}"
    );
    let bundled = file(&before, "wikidata");
    assert_eq!(bundled["origin"], "bundled", "{before}");
    assert_eq!(bundled["license"], "CC0-1.0", "{before}");
    assert!(file(&before, "ut1").is_null(), "{before}");
    assert!(
        holtmail_category_data::is_newer(FIRST, bundled["version"].as_str().unwrap_or_default()),
        "the bundled table is newer than the test release: {before}"
    );

    let first_file = table(
        "wikidata",
        FIRST,
        &[
            (SHOP, Topic::Purchases),
            ("trips.example.net", Topic::Travel),
        ],
    );
    let first = Release::new(
        FIRST,
        vec![(WIKIDATA, first_file.clone(), 2)],
        now() + 30 * DAY,
        TEST_KEY_ID,
        &test_key(),
    );
    let published = Instant::now();
    first.publish(&release_dir);
    let loaded = wait_data(&api, "the first release loaded", |d| {
        file(d, "wikidata")["version"] == FIRST
    })
    .await;
    let took_load = published.elapsed();
    let wikidata = file(&loaded, "wikidata");
    assert_eq!(wikidata["origin"], "downloaded", "{loaded}");
    assert_eq!(wikidata["license"], "CC0-1.0", "{loaded}");
    assert_eq!(wikidata["file"], WIKIDATA, "{loaded}");
    assert_eq!(wikidata["rows"], 2, "{loaded}");
    assert!(wikidata["attribution"].is_null(), "{loaded}");
    wait_topics(&api, &shop, "purchases after the update", |t| {
        has_topic(t, "purchases")
    })
    .await;
    let took_topic = published.elapsed();
    let listed = api
        .get(&format!(
            "/api/archives/{archive}/messages?category=purchases&limit=50"
        ))
        .await
        .ok();
    let listed: Vec<String> = listed["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(id)
        .collect();
    assert_eq!(listed, vec![shop.clone()]);

    let copy = stored(&container, &dir.join("stored-1"));
    assert_eq!(
        std::fs::read(copy.join(WIKIDATA)).ok(),
        Some(first_file.clone())
    );
    assert_eq!(
        std::fs::read(copy.join("manifest.json")).ok(),
        Some(first.manifest.clone())
    );
    assert_eq!(
        std::fs::read(copy.join("manifest.json.sig")).ok(),
        Some(first.signature.clone())
    );

    let manifest_path = format!("/{run}/manifest.json");
    let seen = wait_fetches(&format!("/{run}/"), "a 304 after the 200", |seen| {
        of(seen, DATA_NAME, &manifest_path)
            .iter()
            .any(|f| f.status == 304)
    })
    .await;
    let manifest = of(&seen, DATA_NAME, &manifest_path);
    let fetched = manifest
        .iter()
        .position(|f| f.status == 200)
        .expect("a 200 for the manifest");
    assert!(
        manifest[..fetched].iter().all(|f| f.status == 404),
        "{manifest:#?}"
    );
    assert!(manifest[fetched].if_none_match.is_empty(), "{manifest:#?}");
    let etag = manifest[fetched].etag.clone();
    assert!(!etag.is_empty(), "{manifest:#?}");
    let not_modified = &manifest[fetched + 1];
    assert_eq!(not_modified.status, 304, "{manifest:#?}");
    assert_eq!(not_modified.if_none_match, etag, "{manifest:#?}");
    assert_eq!(
        of(&seen, DATA_NAME, &format!("/{run}/{WIKIDATA}"))
            .iter()
            .map(|f| f.status)
            .collect::<Vec<_>>(),
        vec![200],
        "the data file once"
    );
    assert_eq!(
        of(&seen, DATA_NAME, &format!("/{run}/manifest.json.sig")).len(),
        1,
        "the signature once"
    );
    let after = wait_data(&api, "not modified", |d| {
        d["updates"]["last_outcome"] == "not_modified"
    })
    .await;
    assert!(after["updates"]["last_checked_at"].is_i64(), "{after}");
    let evidence = format!(
        "200 etag {etag} then 304 for If-None-Match {}",
        not_modified.if_none_match
    );

    let refused_cases = [
        (
            "a stranger's key",
            Release::new(
                SECOND,
                vec![(WIKIDATA, first_file.clone(), 2)],
                now() + 30 * DAY,
                TEST_KEY_ID,
                &stranger_key(),
            ),
            "signature",
        ),
        (
            "an expired manifest",
            Release::new(
                SECOND,
                vec![(WIKIDATA, first_file.clone(), 2)],
                now() - 3600,
                TEST_KEY_ID,
                &test_key(),
            ),
            "expired",
        ),
        (
            "an older version",
            Release::new(
                OLDER,
                vec![(WIKIDATA, first_file.clone(), 2)],
                now() + 30 * DAY,
                TEST_KEY_ID,
                &test_key(),
            ),
            "not_newer",
        ),
    ];
    let mut since = refusals(&container).len();
    for (what, release, reason) in &refused_cases {
        tokio::time::sleep(Duration::from_millis(1100)).await;
        release.publish_manifest(&release_dir);
        since = wait_refused(&container, since, reason).await;
        let now_data = wait_data(&api, what, |d| d["updates"]["last_outcome"] == "refused").await;
        assert_eq!(
            file(&now_data, "wikidata")["version"],
            FIRST,
            "{what}: {now_data}"
        );
        assert_eq!(
            file(&now_data, "wikidata")["origin"],
            "downloaded",
            "{what}: {now_data}"
        );
    }
    let copy = stored(&container, &dir.join("stored-2"));
    assert_eq!(
        std::fs::read(copy.join("manifest.json")).ok(),
        Some(first.manifest.clone())
    );
    assert_eq!(
        std::fs::read(copy.join(WIKIDATA)).ok(),
        Some(first_file.clone())
    );
    assert_eq!(
        of(
            &fetches(&format!("/{run}/")),
            DATA_NAME,
            &format!("/{run}/{WIKIDATA}")
        )
        .len(),
        1,
        "a refused manifest fetched the data file"
    );
    let topics = api.message(&shop).await["insights"]["topics"].clone();
    assert!(has_topic(&topics, "purchases"), "{topics}");

    container.stop();
    config(
        &config_file,
        &format!("https://{DATA_NAME}/hop/{run}/"),
        &["wikidata", "ut1"],
    );
    let third_wikidata = table("wikidata", THIRD, &[(SHOP, Topic::Purchases)]);
    let third_ut1 = table("ut1", THIRD, &[(JOBS, Topic::Jobs)]);
    let third = Release::new(
        THIRD,
        vec![
            (WIKIDATA, third_wikidata.clone(), 1),
            (UT1, third_ut1.clone(), 1),
        ],
        now() + 30 * DAY,
        TEST_KEY_ID,
        &test_key(),
    );
    third.publish(&release_dir);
    container.start_again().await;
    let api = owner(&container).await;
    let loaded = wait_data(&api, "the third release through the mirror", |d| {
        file(d, "wikidata")["version"] == THIRD && file(d, "ut1")["version"] == THIRD
    })
    .await;
    let ut1 = file(&loaded, "ut1");
    assert_eq!(ut1["origin"], "downloaded", "{loaded}");
    assert_eq!(ut1["license"], "CC-BY-SA-4.0", "{loaded}");
    assert_eq!(ut1["file"], UT1, "{loaded}");
    assert!(
        ut1["attribution"]
            .as_str()
            .is_some_and(|a| a.contains("UT1") && a.contains("CC BY-SA 4.0")),
        "{loaded}"
    );
    assert_eq!(
        loaded["updates"]["data_sources"],
        serde_json::json!(["wikidata", "ut1"]),
        "{loaded}"
    );
    wait_topics(&api, &jobs, "jobs from UT1", |t| has_topic(t, "jobs")).await;
    let seen = fetches(&format!("/{run}/"));
    let hopped = fetches(&format!("/hop/{run}/"));
    assert!(
        of(&hopped, DATA_NAME, &format!("/hop/{run}/manifest.json"))
            .iter()
            .any(|f| f.status == 302),
        "{hopped:#?}"
    );
    for name in ["manifest.json", "manifest.json.sig", WIKIDATA, UT1] {
        assert!(
            of(&seen, MIRROR_NAME, &format!("/{run}/{name}"))
                .iter()
                .any(|f| f.status == 200),
            "{name} from the mirror: {seen:#?}"
        );
    }
    let logs = container.logs();
    assert!(
        logs.contains("stored category data file loaded"),
        "the stored release did not load at start:\n{logs}"
    );

    container.stop();
    config(
        &config_file,
        &format!("https://{DATA_NAME}/away/{run}/"),
        &["wikidata", "ut1"],
    );
    let fourth_file = table("wikidata", FOURTH, &[(SHOP, Topic::Purchases)]);
    Release::new(
        FOURTH,
        vec![(WIKIDATA, fourth_file, 1)],
        now() + 30 * DAY,
        TEST_KEY_ID,
        &test_key(),
    )
    .publish(&release_dir);
    let since = refusals(&container).len();
    container.start_again().await;
    let api = owner(&container).await;
    wait_refused(&container, since, "download_refused").await;
    let now_data = wait_data(&api, "the unpinned redirect refused", |d| {
        d["updates"]["last_outcome"] == "refused"
    })
    .await;
    assert_eq!(file(&now_data, "wikidata")["version"], THIRD, "{now_data}");
    assert_eq!(file(&now_data, "ut1")["version"], THIRD, "{now_data}");
    let away = fetches(&format!("/away/{run}/"));
    assert!(
        of(&away, DATA_NAME, &format!("/away/{run}/manifest.json"))
            .iter()
            .all(|f| f.status == 302)
            && !away.is_empty(),
        "{away:#?}"
    );
    assert!(
        requests(&format!("/{run}/")).is_empty(),
        "{OUTSIDE_NAME} was contacted"
    );

    let logs = container.logs();
    for line in ["category data updated", "category data not modified"] {
        assert!(logs.contains(line), "no {line:?} logged:\n{logs}");
    }
    let hashes: Vec<String> = [&first, &third]
        .iter()
        .flat_map(|r| {
            holtmail_category_data::Manifest::parse(&r.manifest)
                .expect("a manifest")
                .files
                .into_iter()
                .map(|f| f.sha256)
        })
        .collect();
    let seed = std::fs::read_to_string(
        holtmail_integration::workspace_root()
            .join("crates/category-domains/tests/fixtures/test-signing-key.hex"),
    )
    .expect("the seed");
    LogScanner::new()
        .secret("a row's domain", SHOP)
        .secret("a row's domain", JOBS)
        .secret("a row's domain", "trips.example.net")
        .secrets("a file hash", hashes.iter().map(String::as_str))
        .secret("the signing seed", seed.trim())
        .secret("a subject token", &token)
        .check(&logs);
    eprintln!(
        "first release loaded {took_load:?} after publishing, topic re-derived after {took_topic:?}; {evidence}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&release_dir);
}
