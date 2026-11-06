//! Message insights on stored mail: 1 500 invented messages imported while insights were
//! off, then the walk over stored mail started by a restart with them on, killed with `SIGKILL`
//! mid-walk and resumed at the next start. Every message ends with an insights row, rows written
//! before the kill are not computed again, the reader shows the code and the unsubscribe link,
//! and neither the logs nor `holtmail insights explain` carry a code or a link.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use holtmail_integration::api::{OWNER_EMAIL, OWNER_PASSWORD};
use holtmail_integration::data::{Message, mbox, now};
use holtmail_integration::{Api, Holtmail, LogScanner, Options, require_it, unique};
use serde_json::Value;

const COUNT: usize = 1_500;
const RECIPIENT: &str = "ada@example.org";

struct Seeded {
    messages: Vec<Message>,
    codes: Vec<(String, String)>,
    unsubscribes: Vec<(String, String)>,
    personal: Vec<String>,
}

fn seeded() -> Seeded {
    let mut seeded = Seeded {
        messages: Vec::new(),
        codes: Vec::new(),
        unsubscribes: Vec::new(),
        personal: Vec::new(),
    };
    let filler =
        "Invented paragraph about the weekly garden club, the library hours and the bake sale. "
            .repeat(8);
    for i in 0..COUNT {
        let token = unique("ins");
        let base = Message::new(RECIPIENT);
        let message = match i % 5 {
            0 => {
                let code = (300_000 + (i * 997) % 600_000).to_string();
                let mut message = base
                    .subject("Your Example sign-in code")
                    .body(&format!(
                        "Hi Ada,\n\nYour verification code is:\n\n{code}\n\nIt expires in 10 minutes. If you did not request it, you can ignore this message.\n\nThe Example team\n"
                    ));
                message.from = "no-reply@accounts.example.com".into();
                message.from_name = "Example Accounts".into();
                seeded.codes.push((token.clone(), code));
                message
            }
            1 | 2 => {
                let link = format!("https://lists.example.com/leave/{token}");
                let paragraphs: String = (0..24)
                    .map(|n| format!("<p>Story {n}: {filler}</p>\n"))
                    .collect();
                let mut message = base
                    .subject("Example News this week")
                    .header(&format!(
                        "List-Unsubscribe: <{link}>, <mailto:leave@lists.example.com?subject=leave>"
                    ))
                    .header("List-Unsubscribe-Post: List-Unsubscribe=One-Click")
                    .header("List-Id: Example News <news.lists.example.com>")
                    .html(&format!(
                        "<html><body><h1>Example News</h1>\n{paragraphs}<p><a href=\"{link}\">Leave this list</a></p></body></html>\n"
                    ));
                message.from = "news@lists.example.com".into();
                message.from_name = "Example News".into();
                seeded.unsubscribes.push((token.clone(), link));
                message
            }
            _ => {
                seeded.personal.push(token.clone());
                base.subject("Lunch on Thursday?").body(&format!(
                    "Hi Ada,\n\nShall we meet for lunch on Thursday?\n\n{filler}\n\nBo\n"
                ))
            }
        };
        let mut message = message;
        message.message_id = format!("{token}@example.org");
        seeded.messages.push(message);
    }
    seeded
}

async fn count(holtmail: &Holtmail, sql: &'static str) -> usize {
    holtmail.query(sql, &[]).await[0][0]
        .parse()
        .expect("a count")
}

async fn message_id(holtmail: &Holtmail, token: &str) -> String {
    let pattern = format!("%{token}%");
    let rows = holtmail
        .query(
            "SELECT id FROM messages WHERE rfc_message_id LIKE ?",
            &[&pattern],
        )
        .await;
    assert_eq!(rows.len(), 1, "one message for {token}");
    rows[0][0].clone()
}

async fn computed(holtmail: &Holtmail) -> BTreeMap<String, String> {
    holtmail
        .query("SELECT message_id, computed_at FROM message_insights", &[])
        .await
        .into_iter()
        .map(|row| (row[0].clone(), row[1].clone()))
        .collect()
}

async fn analyse_tasks(api: &Api) -> Vec<Value> {
    let mut tasks: Vec<Value> = api
        .tasks()
        .await
        .into_iter()
        .filter(|t| t["kind"] == "analyse")
        .collect();
    tasks.sort_by_key(|t| t["started_at"].as_i64().unwrap_or(0));
    tasks
}

#[tokio::test(flavor = "multi_thread")]
async fn the_walk_fills_insights_and_resumes_after_a_kill() {
    require_it!();
    let seeded = seeded();
    let mut holtmail = Holtmail::start(Options {
        extra_config: "[insights]\nenabled = false\n".into(),
        ..Default::default()
    })
    .await;
    let api = Api::setup_owner(&holtmail).await;
    let archive = api.create_archive("Insights").await;
    let (_, task) = api
        .import_within(
            &archive,
            "insights.mbox",
            mbox(&seeded.messages),
            Duration::from_secs(600),
        )
        .await;
    assert_eq!(task["state"], "done", "{task}");
    assert_eq!(
        count(&holtmail, "SELECT COUNT(*) FROM messages").await,
        COUNT
    );
    assert_eq!(
        count(&holtmail, "SELECT COUNT(*) FROM message_insights").await,
        0,
        "insights computed while turned off"
    );
    let off = api
        .get(&format!("/api/archives/{archive}/insights/status"))
        .await;
    assert_eq!(off.status, 404, "{}", off.body);
    let (otp_token, otp_code) = &seeded.codes[seeded.codes.len() / 2];
    let otp = message_id(&holtmail, otp_token).await;
    assert!(api.message(&otp).await["insights"].is_null());
    assert!(analyse_tasks(&api).await.is_empty());

    holtmail.stop().await;
    let config = std::fs::read_to_string(holtmail.config_path()).expect("the config");
    std::fs::write(
        holtmail.config_path(),
        config.replace("enabled = false", "enabled = true"),
    )
    .expect("the config is written");

    holtmail.start_again().await;
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let stored = count(
            &holtmail,
            "SELECT COUNT(*) FROM tasks WHERE kind = 'analyse' AND state = 'running'",
        )
        .await;
        let written = count(&holtmail, "SELECT COUNT(*) FROM message_insights").await;
        if stored == 1 && written >= 10 {
            break;
        }
        assert!(Instant::now() < deadline, "the walk did not start");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    holtmail.kill();
    let before = computed(&holtmail).await;
    assert!(
        before.len() < COUNT,
        "the walk finished before it could be killed; raise COUNT"
    );
    eprintln!(
        "{} of {COUNT} messages had insights at the kill",
        before.len()
    );

    let since = now().timestamp_millis();
    holtmail.start_again().await;
    let api = Api::sign_in(&holtmail, OWNER_EMAIL, OWNER_PASSWORD).await;
    let deadline = Instant::now() + Duration::from_secs(300);
    let resumed = loop {
        let status = api
            .get(&format!("/api/archives/{archive}/insights/status"))
            .await
            .ok();
        let finished = analyse_tasks(&api)
            .await
            .into_iter()
            .find(|t| t["started_at"].as_i64().unwrap_or(0) >= since && t["state"] != "running");
        if status["pending"] == 0
            && status["running"] == false
            && let Some(task) = finished
        {
            break task;
        }
        assert!(
            Instant::now() < deadline,
            "the walk did not finish: {status}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(resumed["state"], "done", "{resumed}");
    assert_eq!(
        resumed["done"].as_u64(),
        Some((COUNT - before.len()) as u64),
        "the resumed walk read messages that already had insights: {resumed}"
    );
    let tasks = analyse_tasks(&api).await;
    assert_eq!(tasks.len(), 2, "{tasks:?}");
    assert_eq!(tasks[0]["state"], "interrupted", "{}", tasks[0]);

    let after = computed(&holtmail).await;
    assert_eq!(after.len(), COUNT);
    assert_eq!(
        count(
            &holtmail,
            "SELECT COUNT(*) FROM messages m LEFT JOIN message_insights i ON i.message_id = m.id WHERE i.message_id IS NULL",
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &holtmail,
            "SELECT COUNT(*) FROM message_insights WHERE outcome <> 'ok'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &holtmail,
            "SELECT COUNT(DISTINCT fingerprint) FROM message_insights"
        )
        .await,
        1
    );
    for (id, at) in &before {
        assert_eq!(
            after.get(id),
            Some(at),
            "{id} was computed again after the restart"
        );
    }

    let detail = api.message(&otp).await;
    let insights = &detail["insights"];
    assert_eq!(insights["status"], "ready", "{insights}");
    let codes: Vec<&str> = insights["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|i| i["type"] == "code")
        .filter_map(|i| i["value"].as_str())
        .collect();
    assert_eq!(codes, [otp_code.as_str()], "{insights}");

    let (list_token, link) = &seeded.unsubscribes[0];
    let list = api.message(&message_id(&holtmail, list_token).await).await;
    let insights = &list["insights"];
    assert_eq!(insights["status"], "ready", "{insights}");
    assert_eq!(
        insights["unsubscribe"]["https"],
        link.as_str(),
        "{insights}"
    );
    assert_eq!(
        insights["unsubscribe"]["mailto"], "mailto:leave@lists.example.com?subject=leave",
        "{insights}"
    );
    assert!(
        insights["items"]
            .as_array()
            .expect("items")
            .iter()
            .all(|i| i["type"] != "code"),
        "{insights}"
    );

    let personal = api
        .message(&message_id(&holtmail, &seeded.personal[0]).await)
        .await;
    let insights = &personal["insights"];
    assert_eq!(insights["status"], "ready", "{insights}");
    assert_eq!(insights["items"], serde_json::json!([]), "{insights}");
    assert!(insights["unsubscribe"].is_null(), "{insights}");

    let explained = holtmail.command(&["insights", "explain", &otp], "");
    assert!(explained.status.success(), "{explained:?}");
    let explained = String::from_utf8_lossy(&explained.stdout);
    assert!(!explained.trim().is_empty());
    assert_no_code(&explained, &[otp_code.as_str()]);
    LogScanner::new()
        .secret("the sender", "no-reply@accounts.example.com")
        .secret("the recipient", RECIPIENT)
        .check(&explained);

    let logs = holtmail.logs();
    assert_no_code(
        &logs,
        &seeded
            .codes
            .iter()
            .map(|(_, c)| c.as_str())
            .collect::<Vec<_>>(),
    );
    LogScanner::new()
        .secrets(
            "an unsubscribe link",
            seeded.unsubscribes.iter().map(|(_, l)| l.as_str()),
        )
        .secret("the list host", "lists.example.com")
        .secret("the recipient", RECIPIENT)
        .check(&logs);
}

#[track_caller]
fn assert_no_code(text: &str, codes: &[&str]) {
    for line in text.lines() {
        for run in line.split(|c: char| !c.is_ascii_digit()) {
            assert!(!codes.contains(&run), "a seeded code appears: {line}");
        }
    }
}
