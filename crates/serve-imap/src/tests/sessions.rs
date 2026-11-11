use std::time::Duration;

use holtmail_mailview::testing::{Scenario, mail};

use super::harness::*;
use crate::Settings;

#[tokio::test]
async fn greets_with_sign_in_capabilities() {
    let instance = Instance::new();
    let client = instance.connect().await;
    assert!(client.greeting.starts_with("* OK [CAPABILITY IMAP4REV1"));
    assert!(client.greeting.contains("AUTH=PLAIN"));
    assert!(client.greeting.contains("SASL-IR"));
    assert!(!client.greeting.contains("STARTTLS"));
}

#[tokio::test]
async fn refuses_passwords_in_the_clear() {
    let instance = Instance::new();
    let mut client = instance.connect_with(false, true).await;
    assert!(
        client.greeting.contains("LOGINDISABLED"),
        "{}",
        client.greeting
    );
    assert!(client.greeting.contains("STARTTLS"));
    assert!(!client.greeting.contains("AUTH=PLAIN"));
    let answer = client.run(&format!("a LOGIN {EDITOR} {SECRET}")).await;
    assert!(
        status(&answer).contains("a NO [PRIVACYREQUIRED]"),
        "{answer}"
    );
    let answer = client
        .run("b AUTHENTICATE PLAIN AGFkYUBleGFtcGxlLmNvbQBobV9pbnZlbnRlZA==")
        .await;
    assert!(
        status(&answer).contains("b NO [PRIVACYREQUIRED]"),
        "{answer}"
    );
}

#[tokio::test]
async fn starttls_drops_what_was_sent_with_it_in_the_clear() {
    let instance = Instance::new();
    let mut client = instance.connect_with(false, true).await;
    client
        .send_raw(b"a STARTTLS\r\nb LOGIN ada@example.com hm_invented\r\n")
        .await;
    assert_eq!(client.line().await, "a OK Begin TLS negotiation now\r\n");
    let answer = client.run("c CAPABILITY").await;
    assert!(
        answer.starts_with("* CAPABILITY"),
        "the injected command was run: {answer}"
    );
    assert!(answer.contains("AUTH=PLAIN"));
    assert!(!answer.contains("STARTTLS"));
    client.login(EDITOR).await;
}

#[tokio::test]
async fn wrong_secrets_and_commands_before_sign_in() {
    let instance = Instance::new();
    let mut client = instance.connect().await;
    let answer = client.run(&format!("a LOGIN {EDITOR} hm_wrong")).await;
    assert!(
        status(&answer).starts_with("a NO [AUTHENTICATIONFAILED]"),
        "{answer}"
    );
    let answer = client.run("b SELECT INBOX").await;
    assert!(status(&answer).starts_with("b BAD"), "{answer}");
    let answer = client
        .run("c AUTHENTICATE PLAIN Ym9iQGV4YW1wbGUuY29tAGFkYUBleGFtcGxlLmNvbQBobV9pbnZlbnRlZA==")
        .await;
    assert!(
        status(&answer).starts_with("c NO [AUTHENTICATIONFAILED]"),
        "another authorization identity was accepted: {answer}"
    );
}

#[tokio::test]
async fn a_large_literal_before_sign_in_ends_the_connection() {
    let instance = Instance::new();
    let mut client = instance.connect().await;
    client.send("a LOGIN {1000000}").await;
    let go = client.line().await;
    assert!(go.starts_with('+'), "{go}");
    client.send_raw(&vec![b'x'; 32 << 10]).await;
    let rest = client.closed().await;
    assert!(rest.contains("* BYE"), "{rest}");
}

#[tokio::test]
async fn sign_in_has_one_deadline_for_the_whole_exchange() {
    let instance = Instance::with(Options {
        settings: Settings {
            sign_in_timeout: Duration::from_millis(600),
            ..Settings::default()
        },
        ..Options::default()
    });
    let started = std::time::Instant::now();
    let mut client = instance.connect().await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let answer = client.run("n1 NOOP").await;
    assert!(status(&answer).starts_with("n1 OK"), "{answer}");
    let rest = client.closed().await;
    assert!(rest.contains("BYE"), "{rest}");
    assert!(
        started.elapsed() < Duration::from_millis(900),
        "a command restarted the sign-in wait"
    );
}

#[tokio::test]
async fn id_names_the_instance_without_a_version() {
    let instance = Instance::new();
    let mut client = instance.connect().await;
    let answer = client.run("a ID (\"name\" \"Thunderbird\")").await;
    assert!(
        answer.contains("* ID (\"name\" \"Holtmail\" \"holtmail-instance\" \"instance-token\")"),
        "{answer}"
    );
    assert!(!answer.to_ascii_lowercase().contains("version"));
}

#[tokio::test]
async fn capabilities_after_sign_in() {
    let instance = Instance::new();
    let mut client = instance.connect().await;
    let answer = client.run(&format!("a LOGIN {EDITOR} {SECRET}")).await;
    for capability in [
        "IDLE",
        "UIDPLUS",
        "MOVE",
        "UNSELECT",
        "CONDSTORE",
        "QRESYNC",
        "LITERAL-",
        "NAMESPACE",
        "SPECIAL-USE",
        "CHILDREN",
        "APPENDLIMIT=10485760",
    ] {
        assert!(
            answer.contains(capability),
            "{capability} missing: {answer}"
        );
    }
    assert!(!answer.contains("AUTH=PLAIN"));
}

#[tokio::test]
async fn lists_the_tree_with_special_use_and_subscriptions() {
    let instance = Instance::new();
    let mut client = instance.connect().await;
    client.login(EDITOR).await;
    let answer = client.run("a LIST \"\" \"*\"").await;
    assert!(answer.contains("* LIST (\\HasNoChildren \\Sent) \"/\" Sent"));
    assert!(answer.contains("* LIST (\\HasNoChildren \\All) \"/\" \"All Mail\""));
    assert!(answer.contains("* LIST (\\Noselect \\HasChildren) \"/\" Accounts/Work"));
    assert!(answer.contains("* LIST (\\HasNoChildren) \"/\" INBOX"));
    let answer = client.run("b LIST \"\" \"%\"").await;
    assert!(!answer.contains("Accounts/Work"), "{answer}");
    assert!(answer.contains("\"/\" Accounts\r\n"));
    let answer = client.run("c LIST \"Accounts/\" \"%\"").await;
    assert!(answer.contains("\"/\" Accounts/Work\r\n"), "{answer}");
    let answer = client.run("d LIST \"\" \"\"").await;
    assert!(
        answer.contains("* LIST (\\Noselect) \"/\" \"\""),
        "{answer}"
    );
    let answer = client.run("e LSUB \"\" \"*\"").await;
    assert!(answer.contains("* LSUB () \"/\" INBOX"));
    assert!(
        !answer.contains("All Mail"),
        "All Mail is subscribed by default: {answer}"
    );
    let answer = client.run("f SUBSCRIBE \"All Mail\"").await;
    assert!(status(&answer).starts_with("f OK"));
    let answer = client.run("g LSUB \"\" \"All*\"").await;
    assert!(answer.contains("\"All Mail\""), "{answer}");
    let answer = client.run("h CREATE Projects").await;
    assert!(status(&answer).starts_with("h NO [CANNOT]"));
}

#[tokio::test]
async fn non_ascii_names_go_out_in_modified_utf7() {
    let mut scenario = Scenario::one_account();
    scenario.accounts[0].folders = &[
        ("INBOX", Some(holtmail_mailview::FolderRole::Inbox)),
        ("Отправленные", None),
    ];
    for message in &mut scenario.messages {
        message.places.retain(|p| p.folder == "INBOX");
    }
    scenario.messages.retain(|m| !m.places.is_empty());
    let instance = Instance::with(Options {
        scenario,
        ..Options::default()
    });
    let mut client = instance.connect().await;
    client.login(EDITOR).await;
    let answer = client.run("a LIST \"\" \"*\"").await;
    assert!(
        answer.contains("&BB4EQgQ,BEAEMAQyBDsENQQ9BD0ESwQ1-"),
        "{answer}"
    );
    let answer = client
        .run("b SELECT &BB4EQgQ,BEAEMAQyBDsENQQ9BD0ESwQ1-")
        .await;
    assert!(status(&answer).starts_with("b OK"), "{answer}");
}

#[tokio::test]
async fn status_and_examine_read_without_changing() {
    let instance = Instance::new();
    let mut client = instance.connect().await;
    client.login(EDITOR).await;
    let answer = client
        .run("a STATUS INBOX (MESSAGES RECENT UIDNEXT UIDVALIDITY UNSEEN HIGHESTMODSEQ)")
        .await;
    assert!(answer.contains("MESSAGES 3 RECENT 0 UIDNEXT 4"), "{answer}");
    assert!(answer.contains("UNSEEN 2"));
    let answer = client.run("b EXAMINE INBOX").await;
    assert!(answer.contains("[PERMANENTFLAGS ()]"), "{answer}");
    assert!(status(&answer).starts_with("b OK [READ-ONLY]"));
    let answer = client.run("c STORE 1 +FLAGS (\\Seen)").await;
    assert!(status(&answer).starts_with("c NO"), "{answer}");
    let answer = client.run("d STATUS Nowhere (MESSAGES)").await;
    assert!(status(&answer).starts_with("d NO [NONEXISTENT]"));
}

#[tokio::test]
async fn fetches_bodies_sections_and_partials() {
    let instance = Instance::new();
    let mut client = instance.connect().await;
    client.login(EDITOR).await;
    client.run("a SELECT \"Accounts/Work/Projects/2026\"").await;
    let answer = client
        .run("b FETCH 1 (BODY.PEEK[1] BODY.PEEK[2.MIME] BODY.PEEK[TEXT]<0.6> BODY.PEEK[]<0.5>)")
        .await;
    assert!(
        answer.contains("BODY[1] \"Here are the numbers.\""),
        "{answer}"
    );
    assert!(answer.contains("BODY[2.MIME] {"), "{answer}");
    assert!(answer.contains("Content-Disposition: attachment"));
    assert!(answer.contains("BODY[TEXT]<0> {6}\r\n--b1\r\n"), "{answer}");
    assert!(answer.contains("BODY[]<0> \"From:\""), "{answer}");
    let answer = client.run("c FETCH 1 (BODY[9] BODY.PEEK[HEADER.FIELDS.NOT (From To Date Message-ID MIME-Version Content-Type)])").await;
    assert!(answer.contains("BODY[9] \"\""), "{answer}");
    assert!(answer.contains("Subject: Report attached"), "{answer}");
    assert!(!answer.contains("From:"), "{answer}");
    let flags = client.run("d FETCH 1 (FLAGS)").await;
    assert!(
        flags.contains("FLAGS (\\Seen)"),
        "a body fetch without PEEK changed nothing here: {flags}"
    );
}

#[tokio::test]
async fn bare_line_feeds_are_served_as_crlf() {
    let instance = Instance::new();
    let raw = "From: Ada <ada@example.com>\nTo: bob@example.org\nSubject: Imported\nMessage-ID: <lf@example.com>\n\nOne\nTwo\n";
    instance.world.driver.deliver("Work", "INBOX", raw).await;
    let mut client = instance.connect().await;
    client.login(EDITOR).await;
    client.run("a SELECT INBOX").await;
    let answer = client
        .run("b FETCH 4 (RFC822.SIZE BODY.PEEK[] BODYSTRUCTURE)")
        .await;
    let served = raw.replace('\n', "\r\n");
    assert!(
        answer.contains(&format!("RFC822.SIZE {}", served.len())),
        "{answer}"
    );
    assert!(
        answer.contains(&format!("BODY[] {{{}}}\r\n{served}", served.len())),
        "{answer}"
    );
}

#[tokio::test]
async fn implicit_seen_only_when_the_device_asks() {
    let instance = Instance::with(Options {
        implicit_seen: true,
        ..Options::default()
    });
    let mut client = instance.connect().await;
    client.login(EDITOR).await;
    client.run("a SELECT INBOX").await;
    let answer = client.run("b FETCH 1 (BODY.PEEK[])").await;
    assert!(!answer.contains("FLAGS"), "{answer}");
    let answer = client.run("c FETCH 1 (BODY[])").await;
    assert!(answer.contains("FLAGS (\\Seen)"), "{answer}");
}

#[tokio::test]
async fn search_by_flags_text_and_uids() {
    let instance = Instance::new();
    let mut client = instance.connect().await;
    client.login(EDITOR).await;
    client.run("a SELECT INBOX").await;
    let answer = client.run("b SEARCH UNSEEN").await;
    assert!(answer.contains("* SEARCH 1 3\r\n"), "{answer}");
    let answer = client.run("c UID SEARCH SUBJECT welcome").await;
    assert!(answer.contains("* SEARCH 1 3\r\n"), "{answer}");
    let answer = client.run("d SEARCH OR FLAGGED FROM bob UID 2:*").await;
    assert!(answer.contains("* SEARCH 2 3\r\n"), "{answer}");
    let answer = client.run("e SEARCH CHARSET UTF-8 TEXT numbers").await;
    assert!(answer.contains("* SEARCH 2\r\n"), "{answer}");
    let answer = client.run("f SEARCH CHARSET KOI8-R TEXT numbers").await;
    assert!(status(&answer).starts_with("f NO [BADCHARSET"), "{answer}");
    let answer = client.run("g SEARCH MODSEQ 1").await;
    assert!(answer.contains("(MODSEQ "), "{answer}");
    let answer = client.run("h SEARCH SINCE 3-Nov-2026 NOT SEEN").await;
    assert!(answer.contains("* SEARCH 3\r\n"), "{answer}");
}

#[tokio::test]
async fn store_reports_flags_and_modseq() {
    let instance = Instance::new();
    let mut client = instance.connect().await;
    client.login(EDITOR).await;
    client.run("a ENABLE CONDSTORE").await;
    let select = client.run("b SELECT INBOX").await;
    assert!(select.contains("HIGHESTMODSEQ"));
    let answer = client.run("c STORE 1 +FLAGS (\\Flagged $Forwarded)").await;
    assert!(
        answer.contains("* 1 FETCH (FLAGS ($Forwarded \\Flagged) UID 1 MODSEQ ("),
        "{answer}"
    );
    let answer = client.run("d UID STORE 1 -FLAGS.SILENT ($Forwarded)").await;
    assert!(answer.contains("* 1 FETCH (UID 1 MODSEQ ("), "{answer}");
    assert!(!answer.contains("FLAGS ("), "{answer}");
    let answer = client.run("e STORE 1 FLAGS (\\Seen)").await;
    assert!(answer.contains("FLAGS (\\Seen)"), "{answer}");
    let answer = client.run("f FETCH 1 (FLAGS MODSEQ)").await;
    assert!(answer.contains("FLAGS (\\Seen)"), "{answer}");
}

#[tokio::test]
async fn unchanged_since_refuses_what_changed() {
    let instance = Instance::new();
    let mut client = instance.connect().await;
    client.login(EDITOR).await;
    client.run("a SELECT INBOX").await;
    let answer = client
        .run("b UID STORE 1:3 (UNCHANGEDSINCE 1) +FLAGS (\\Flagged)")
        .await;
    assert!(status(&answer).contains("b OK [MODIFIED 1:3]"), "{answer}");
    let fetched = client.run("c FETCH 1 (MODSEQ)").await;
    let start = fetched.find("MODSEQ (").unwrap() + 8;
    let modseq: u64 = fetched[start..start + fetched[start..].find(')').unwrap()]
        .parse()
        .unwrap();
    let answer = client
        .run(&format!(
            "d STORE 1 (UNCHANGEDSINCE {modseq}) +FLAGS (\\Flagged)"
        ))
        .await;
    assert!(status(&answer).starts_with("d OK STORE"), "{answer}");
    assert!(answer.contains("\\Flagged"));
}

#[tokio::test]
async fn deleted_and_expunge_become_held_deletes() {
    let instance = Instance::new();
    let mut client = instance.connect().await;
    client.login(EDITOR).await;
    client.run("a SELECT INBOX").await;
    let answer = client.run("b STORE 1 +FLAGS.SILENT (\\Deleted)").await;
    assert!(status(&answer).starts_with("b OK"), "{answer}");
    let answer = client.run("c EXPUNGE").await;
    assert!(answer.contains("* 1 EXPUNGE\r\n"), "{answer}");
    assert!(status(&answer).starts_with("c OK"));
    let answer = client.run("d FETCH 1:* (UID)").await;
    assert!(!answer.contains("UID 1)"), "{answer}");
    let trash = client.run("e STATUS Trash (MESSAGES)").await;
    assert!(
        trash.contains("MESSAGES 1"),
        "the delete went to the trash: {trash}"
    );
    let all = client.run("f STATUS \"All Mail\" (MESSAGES)").await;
    assert!(
        all.contains("MESSAGES 7"),
        "All Mail keeps everything: {all}"
    );
}

#[tokio::test]
async fn read_only_devices_keep_flags_locally_and_cannot_delete() {
    let instance = Instance::new();
    let mut client = instance.connect().await;
    client.login(READER).await;
    let select = client.run("a SELECT INBOX").await;
    assert!(
        select.contains("[PERMANENTFLAGS (\\Seen \\Answered \\Flagged \\Draft \\*)]"),
        "{select}"
    );
    let answer = client.run("b STORE 1 +FLAGS (\\Seen)").await;
    assert!(answer.contains("FLAGS (\\Seen)"), "{answer}");
    assert!(status(&answer).starts_with("b OK"));
    let answer = client.run("c STORE 1 +FLAGS (\\Deleted)").await;
    assert!(status(&answer).starts_with("c NO [CANNOT]"), "{answer}");
    let answer = client.run("d MOVE 1 Trash").await;
    assert!(status(&answer).starts_with("d NO [CANNOT]"), "{answer}");
    client.send("e APPEND INBOX {11}").await;
    assert!(client.line().await.starts_with('+'));
    client.send("Subject: x\r\n").await;
    let answer = client.until("e").await;
    assert!(status(&answer).starts_with("e NO [CANNOT]"), "{answer}");
}

#[tokio::test]
async fn copy_and_move_answer_with_copyuid() {
    let instance = Instance::new();
    let mut client = instance.connect().await;
    client.login(EDITOR).await;
    client.run("a SELECT INBOX").await;
    let answer = client
        .run("b UID COPY 1 \"Accounts/Work/Projects/2026\"")
        .await;
    assert!(status(&answer).starts_with("b OK [COPYUID "), "{answer}");
    let answer = client.run("c UID MOVE 3 Archive").await;
    assert!(status(&answer).starts_with("c NO [CANNOT]"), "{answer}");
    let answer = client.run("d UID MOVE 3 Trash").await;
    assert!(answer.contains("* OK [COPYUID "), "{answer}");
    assert!(answer.contains("* 3 EXPUNGE"), "{answer}");
    assert!(status(&answer).starts_with("d OK"));
    let answer = client.run("e COPY 1 \"All Mail\"").await;
    assert!(status(&answer).starts_with("e NO [CANNOT]"), "{answer}");
    let answer = client.run("f COPY 1 Nowhere").await;
    assert!(status(&answer).starts_with("f NO [TRYCREATE]"), "{answer}");
}

#[tokio::test]
async fn append_answers_with_appenduid_and_limits() {
    let instance = Instance::new();
    let mut client = instance.connect().await;
    client.login(EDITOR).await;
    let message = mail(
        "Ada <ada@example.com>",
        "bob@example.org",
        "Filed",
        "filed",
        "Sat, 07 Nov 2026 09:00:00 +0000",
        "",
        "Filed away.\r\n",
    );
    client
        .send(&format!(
            "a APPEND \"Accounts/Work/Projects/2026\" (\\Seen) {{{}+}}",
            message.len()
        ))
        .await;
    client.send_raw(message.as_bytes()).await;
    client.send("").await;
    let answer = client.until("a").await;
    assert!(status(&answer).starts_with("a OK [APPENDUID "), "{answer}");
    let first = uid_code(&answer, "APPENDUID");
    client
        .send(&format!(
            "b APPEND \"Accounts/Work/Projects/2026\" {{{}+}}",
            message.len()
        ))
        .await;
    client.send_raw(message.as_bytes()).await;
    client.send("").await;
    let again = client.until("b").await;
    assert_eq!(uid_code(&again, "APPENDUID"), first, "{again}");
    let big = "x".repeat(5000);
    client
        .send(&format!("c APPEND INBOX {{{}+}}", big.len()))
        .await;
    client.send_raw(big.as_bytes()).await;
    client.send("").await;
    let answer = client.until("c").await;
    assert!(
        status(&answer).starts_with("c BAD"),
        "LITERAL- took 5000 bytes: {answer}"
    );
    let answer = client.run("d APPEND INBOX {60000000}").await;
    assert!(status(&answer).starts_with("d BAD"), "{answer}");
    client.send("e APPEND \"All Mail\" {5}").await;
    assert!(client.line().await.starts_with('+'));
    client.send("hello").await;
    let answer = client.until("e").await;
    assert!(status(&answer).starts_with("e NO [CANNOT]"), "{answer}");
}

#[tokio::test]
async fn idle_wakes_on_changes_and_keeps_alive() {
    let instance = Instance::with(Options {
        settings: Settings {
            keepalive: Duration::from_millis(200),
            ..Options::default().settings
        },
        ..Options::default()
    });
    let mut client = instance.connect().await;
    client.login(EDITOR).await;
    client.run("a SELECT INBOX").await;
    client.send("b IDLE").await;
    assert_eq!(client.line().await, "+ idling\r\n");
    assert_eq!(client.line().await, "* OK Still here\r\n");
    let raw = mail(
        "Erin <erin@example.org>",
        "bob@example.org",
        "Fresh",
        "fresh",
        "Sat, 07 Nov 2026 10:00:00 +0000",
        "",
        "New.\r\n",
    );
    instance.world.driver.deliver("Work", "INBOX", &raw).await;
    instance.signal.wake();
    let mut line = client.line().await;
    while line.starts_with("* OK Still") {
        line = client.line().await;
    }
    assert_eq!(line, "* 4 EXISTS\r\n");
    client.send("DONE").await;
    let answer = client.until("b").await;
    assert!(
        status(&answer).starts_with("b OK IDLE terminated"),
        "{answer}"
    );
}

#[tokio::test]
async fn revocation_ends_the_session() {
    let instance = Instance::new();
    let mut client = instance.connect().await;
    client.login(EDITOR).await;
    client.run("a SELECT INBOX").await;
    use holtmail_device::DeviceAuth;
    instance
        .devices
        .end_sessions(instance.world.editor.device.unwrap());
    let rest = client.closed().await;
    assert!(rest.contains("* BYE"), "{rest}");
}

#[tokio::test]
async fn revocation_ends_an_idle() {
    let instance = Instance::new();
    let mut client = instance.connect().await;
    client.login(EDITOR).await;
    client.run("a SELECT INBOX").await;
    client.send("b IDLE").await;
    assert_eq!(client.line().await, "+ idling\r\n");
    use holtmail_device::DeviceAuth;
    instance
        .devices
        .end_sessions(instance.world.editor.device.unwrap());
    let rest = client.closed().await;
    assert!(rest.contains("* BYE"), "{rest}");
}

#[tokio::test]
async fn changes_from_elsewhere_arrive_with_the_next_command() {
    let instance = Instance::new();
    let mut first = instance.connect().await;
    first.login(EDITOR).await;
    first.run("a ENABLE CONDSTORE").await;
    first.run("b SELECT INBOX").await;
    let mut second = instance.connect().await;
    second.login(EDITOR).await;
    second.run("a SELECT INBOX").await;
    second.run("b STORE 3 +FLAGS (\\Flagged)").await;
    second.run("c STORE 1 +FLAGS (\\Deleted)").await;
    second.run("d EXPUNGE").await;
    let answer = first.run("c FETCH 2 (FLAGS)").await;
    assert!(
        !answer.contains("EXPUNGE"),
        "EXPUNGE during FETCH: {answer}"
    );
    assert!(answer.contains("FLAGS (\\Flagged)"), "{answer}");
    let answer = first.run("d NOOP").await;
    assert!(answer.contains("* 1 EXPUNGE"), "{answer}");
}

#[tokio::test]
async fn qresync_resyncs_after_expunges() {
    let instance = Instance::new();
    let mut client = instance.connect().await;
    client.login(EDITOR).await;
    client.run("a ENABLE QRESYNC").await;
    let select = client.run("b SELECT INBOX").await;
    let start = select.find("UIDVALIDITY ").unwrap() + 12;
    let validity: u32 = select[start..start + select[start..].find(']').unwrap()]
        .parse()
        .unwrap();
    let start = select.find("HIGHESTMODSEQ ").unwrap() + 14;
    let modseq: u64 = select[start..start + select[start..].find(']').unwrap()]
        .parse()
        .unwrap();
    client.run("c STORE 1 +FLAGS.SILENT (\\Deleted)").await;
    let answer = client.run("d EXPUNGE").await;
    assert!(answer.contains("* VANISHED 1\r\n"), "{answer}");
    client.run("e STORE 2 +FLAGS (\\Flagged)").await;
    client.run("f UNSELECT").await;
    let answer = client
        .run(&format!(
            "g SELECT INBOX (QRESYNC ({validity} {modseq} 1:3))"
        ))
        .await;
    assert!(answer.contains("* VANISHED (EARLIER) 1\r\n"), "{answer}");
    assert!(
        answer.contains("FETCH (UID 3 FLAGS (\\Flagged) MODSEQ"),
        "{answer}"
    );
    let answer = client
        .run(&format!(
            "h UID FETCH 1:* (FLAGS) (CHANGEDSINCE {modseq} VANISHED)"
        ))
        .await;
    assert!(answer.contains("* VANISHED (EARLIER) 1\r\n"), "{answer}");
    let answer = client.run("i SELECT INBOX").await;
    assert!(answer.contains("* OK [CLOSED]"), "{answer}");
}

#[tokio::test]
async fn a_change_the_server_refuses_converges_with_a_new_modseq() {
    let instance = Instance::new();
    let mut client = instance.connect().await;
    client.login(EDITOR).await;
    client.run("a ENABLE CONDSTORE").await;
    client.run("b SELECT INBOX").await;
    let stored = client.run("c STORE 1 +FLAGS (\\Flagged)").await;
    assert!(stored.contains("FLAGS (\\Flagged)"), "{stored}");
    instance.world.driver.settle(false).await;
    let answer = client.run("d NOOP").await;
    assert!(
        answer.contains("* 1 FETCH (UID 1 FLAGS () MODSEQ ("),
        "{answer}"
    );
}

#[tokio::test]
async fn selecting_another_mailbox_says_the_first_closed_without_qresync() {
    let instance = Instance::new();
    let mut client = instance.connect().await;
    client.login(EDITOR).await;
    let first = client.run("a EXAMINE INBOX").await;
    assert!(!first.contains("[CLOSED]"), "{first}");
    let second = client.run("b SELECT INBOX").await;
    assert!(
        second.starts_with("* OK [CLOSED]"),
        "Dovecot's imapc waits for it whenever the server offers QRESYNC: {second}"
    );
}
