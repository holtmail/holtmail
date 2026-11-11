//! Invented transcripts in the shape each client sends, with invented mail.

use holtmail_mailview::testing::mail;

use super::harness::*;

async fn ok(client: &mut Client, command: &str, wanted: &[&str]) -> String {
    let tag = command.split(' ').next().unwrap();
    let answer = client.run(command).await;
    assert!(
        status(&answer).starts_with(&format!("{tag} OK")),
        "{command}\n{answer}"
    );
    for part in wanted {
        assert!(
            answer.contains(part),
            "{command}: {part} missing in\n{answer}"
        );
    }
    answer
}

async fn append(client: &mut Client, command: &str, message: &str) -> String {
    let tag = command.split(' ').next().unwrap().to_string();
    client
        .send(&format!("{command} {{{}}}", message.len()))
        .await;
    assert!(client.line().await.starts_with("+ "));
    client.send_raw(message.as_bytes()).await;
    client.send("").await;
    client.until(&tag).await
}

fn invented(subject: &str, id: &str) -> String {
    mail(
        "Ada Lovelace <ada@example.com>",
        "Bob Builder <bob@example.org>",
        subject,
        id,
        "Sat, 07 Nov 2026 10:00:00 +0000",
        "",
        "Invented body.\r\n",
    )
}

#[tokio::test]
async fn thunderbird() {
    let instance = Instance::new();
    let mut tb = instance.connect().await;
    ok(&mut tb, "1 capability", &["AUTH=PLAIN"]).await;
    tb.send("2 authenticate PLAIN").await;
    assert_eq!(tb.line().await, "+ \r\n");
    tb.send("AGFkYUBleGFtcGxlLmNvbQBobV9pbnZlbnRlZA==").await;
    let answer = tb.until("2").await;
    assert!(status(&answer).starts_with("2 OK [CAPABILITY"), "{answer}");
    ok(
        &mut tb,
        "3 ID (\"name\" \"Thunderbird\" \"version\" \"128.4.0\")",
        &["\"Holtmail\""],
    )
    .await;
    ok(&mut tb, "4 enable CONDSTORE", &["* ENABLED CONDSTORE"]).await;
    ok(
        &mut tb,
        "5 namespace",
        &["* NAMESPACE ((\"\" \"/\")) NIL NIL"],
    )
    .await;
    ok(&mut tb, "6 lsub \"\" \"*\"", &["* LSUB () \"/\" INBOX"]).await;
    ok(&mut tb, "7 list \"\" \"*\"", &["\\Trash) \"/\" Trash"]).await;
    ok(
        &mut tb,
        "8 select \"INBOX\" (CONDSTORE)",
        &["* 3 EXISTS", "[HIGHESTMODSEQ ", "[READ-WRITE]"],
    )
    .await;
    ok(
        &mut tb,
        "9 UID fetch 1:* (FLAGS)",
        &[
            "* 1 FETCH (UID 1 FLAGS ())",
            "* 2 FETCH (UID 2 FLAGS (\\Flagged \\Seen))",
        ],
    )
    .await;
    ok(
        &mut tb,
        "10 UID fetch 3 (UID RFC822.SIZE FLAGS BODY.PEEK[HEADER.FIELDS (From To Cc Bcc Subject Date Message-ID Priority X-Priority References Newsgroups In-Reply-To Content-Type Reply-To)])",
        &["Subject: Re: Welcome aboard", "In-Reply-To: <welcome@example.com>"],
    )
    .await;
    ok(
        &mut tb,
        "11 UID fetch 3 (UID RFC822.SIZE BODY.PEEK[])",
        &["Thanks, happy to be here."],
    )
    .await;
    ok(
        &mut tb,
        "12 UID store 3 +Flags (\\Seen)",
        &["* 3 FETCH (FLAGS (\\Seen) UID 3 MODSEQ ("],
    )
    .await;
    let saved = append(
        &mut tb,
        "13 append \"Drafts\" (\\Seen \\Draft)",
        &invented("Draft one", "draft1"),
    )
    .await;
    assert!(status(&saved).starts_with("13 OK [APPENDUID "), "{saved}");
    let again = append(
        &mut tb,
        "14 append \"Drafts\" (\\Seen \\Draft)",
        &invented("Draft two", "draft1"),
    )
    .await;
    assert_ne!(
        uid_code(&saved, "APPENDUID"),
        uid_code(&again, "APPENDUID"),
        "each autosave is a new UID"
    );
    tb.send("15 IDLE").await;
    assert_eq!(tb.line().await, "+ idling\r\n");
    tb.send("DONE").await;
    let answer = tb.until("15").await;
    assert!(status(&answer).starts_with("15 OK"));
    let bye = tb.run("16 logout").await;
    assert!(bye.starts_with("* BYE"), "{bye}");
}

#[tokio::test]
async fn apple_mail() {
    let instance = Instance::new();
    let mut apple = instance.connect().await;
    ok(
        &mut apple,
        "1.1 ID (\"name\" \"Mac OS X Mail\" \"version\" \"16.0 (3774.600.62)\" \"os\" \"Mac OS X\" \"vendor\" \"Apple Inc.\")",
        &["holtmail-instance"],
    )
    .await;
    ok(
        &mut apple,
        &format!("2.1 LOGIN \"{EDITOR}\" \"{SECRET}\""),
        &[],
    )
    .await;
    ok(&mut apple, "3.1 CAPABILITY", &["SPECIAL-USE", "NAMESPACE"]).await;
    ok(&mut apple, "4.1 NAMESPACE", &["((\"\" \"/\"))"]).await;
    ok(
        &mut apple,
        "5.1 LIST \"\" \"\"",
        &["(\\Noselect) \"/\" \"\""],
    )
    .await;
    ok(
        &mut apple,
        "6.1 LIST \"\" \"*\"",
        &[
            "\\Sent) \"/\" Sent",
            "\\Drafts) \"/\" Drafts",
            "\\Junk) \"/\" Junk",
        ],
    )
    .await;
    ok(
        &mut apple,
        "7.1 SELECT INBOX (CONDSTORE)",
        &["[READ-WRITE]"],
    )
    .await;
    ok(
        &mut apple,
        "8.1 UID FETCH 1:* (FLAGS) (CHANGEDSINCE 1)",
        &["UID 1 FLAGS () MODSEQ ("],
    )
    .await;
    ok(
        &mut apple,
        "9.1 UID FETCH 1:3 (INTERNALDATE UID RFC822.SIZE FLAGS BODY.PEEK[HEADER.FIELDS (date subject from to cc message-id in-reply-to references content-type x-priority list-id list-unsubscribe authentication-results)])",
        &["INTERNALDATE \"02-Nov-2026 09:00:00 +0000\"", "Subject: Quarterly numbers"],
    )
    .await;
    ok(
        &mut apple,
        "10.1 SELECT \"Accounts/Work/Projects/2026\"",
        &["* 1 EXISTS"],
    )
    .await;
    ok(
        &mut apple,
        "11.1 UID FETCH 1 (BODYSTRUCTURE BODY.PEEK[1])",
        &[
            "(\"TEXT\" \"CSV\" (\"NAME\" \"report.csv\") NIL NIL \"BASE64\" 16",
            "\"MIXED\" (\"BOUNDARY\" \"b1\")",
            "BODY[1] \"Here are the numbers.\"",
        ],
    )
    .await;
    ok(&mut apple, "12.1 SELECT INBOX", &[]).await;
    ok(&mut apple, "13.1 UID STORE 1 +FLAGS.SILENT (\\Seen)", &[]).await;
    let moved = ok(
        &mut apple,
        "14.1 UID MOVE 3 Trash",
        &["* OK [COPYUID ", "* 3 EXPUNGE"],
    )
    .await;
    assert!(!moved.contains("* 2 EXPUNGE"), "{moved}");
    let sent = append(
        &mut apple,
        "15.1 APPEND Sent (\\Seen) \"07-Nov-2026 10:00:00 +0000\"",
        &invented("Sent from Apple Mail", "apple-sent"),
    )
    .await;
    assert!(status(&sent).starts_with("15.1 OK [APPENDUID "), "{sent}");
    ok(
        &mut apple,
        "16.1 STATUS Sent (MESSAGES UNSEEN)",
        &["MESSAGES 2"],
    )
    .await;
}

#[tokio::test]
async fn outlook() {
    let instance = Instance::new();
    let mut outlook = instance.connect().await;
    ok(&mut outlook, "A001 CAPABILITY", &["AUTH=LOGIN"]).await;
    outlook.send("A002 AUTHENTICATE LOGIN").await;
    assert_eq!(outlook.line().await, "+ VXNlcm5hbWU6\r\n");
    outlook.send("YWRhQGV4YW1wbGUuY29t").await;
    assert_eq!(outlook.line().await, "+ UGFzc3dvcmQ6\r\n");
    outlook.send("aG1faW52ZW50ZWQ=").await;
    let answer = outlook.until("A002").await;
    assert!(status(&answer).starts_with("A002 OK"), "{answer}");
    ok(&mut outlook, "A003 LIST \"\" \"*\"", &[]).await;
    ok(&mut outlook, "A004 SELECT \"INBOX\"", &["* 3 EXISTS"]).await;
    let all = ok(
        &mut outlook,
        "A005 FETCH 1:* (UID FLAGS)",
        &["* 3 FETCH (UID 3 FLAGS ())"],
    )
    .await;
    assert!(!all.contains("MODSEQ"), "MODSEQ without CONDSTORE: {all}");
    ok(
        &mut outlook,
        "A006 UID FETCH 1:3 (UID RFC822.SIZE FLAGS BODY.PEEK[HEADER])",
        &["BODY[HEADER] {", "Message-ID: <shared@example.com>"],
    )
    .await;
    ok(
        &mut outlook,
        "A007 UID FETCH 1 (UID RFC822.SIZE BODY.PEEK[])",
        &["Glad to have you here."],
    )
    .await;
    ok(
        &mut outlook,
        "A008 UID STORE 1 +FLAGS (\\Deleted)",
        &["\\Deleted"],
    )
    .await;
    ok(&mut outlook, "A009 EXPUNGE", &["* 1 EXPUNGE"]).await;
    ok(
        &mut outlook,
        "A010 STATUS \"Sent\" (MESSAGES UNSEEN)",
        &["MESSAGES 1 UNSEEN 0"],
    )
    .await;
    ok(&mut outlook, "A011 CHECK", &[]).await;
    let bye = outlook.run("A012 LOGOUT").await;
    assert!(bye.contains("A012 OK"));
}

#[tokio::test]
async fn k9_mail() {
    let instance = Instance::new();
    let mut k9 = instance.connect().await;
    ok(&mut k9, "1 CAPABILITY", &["SASL-IR"]).await;
    ok(
        &mut k9,
        "2 AUTHENTICATE PLAIN AGFkYUBleGFtcGxlLmNvbQBobV9pbnZlbnRlZA==",
        &[],
    )
    .await;
    ok(&mut k9, "3 ENABLE QRESYNC", &["* ENABLED QRESYNC"]).await;
    ok(&mut k9, "4 LIST \"\" \"*\"", &[]).await;
    ok(&mut k9, "5 SELECT INBOX", &["[READ-WRITE]"]).await;
    ok(&mut k9, "6 UID SEARCH 1:* NOT DELETED", &["* SEARCH 1 2 3"]).await;
    ok(
        &mut k9,
        "7 UID FETCH 1,3 (UID INTERNALDATE RFC822.SIZE BODY.PEEK[HEADER.FIELDS (date subject from content-type to cc bcc reply-to message-id references in-reply-to list-unsubscribe sender)] FLAGS)",
        &["* 1 FETCH (UID 1 INTERNALDATE", "* 3 FETCH (UID 3"],
    )
    .await;
    ok(
        &mut k9,
        "8 UID FETCH 1 (UID BODY.PEEK[TEXT]<0.4096>)",
        &["BODY[TEXT]<0> {24}\r\nGlad to have you here.\r\n"],
    )
    .await;
    ok(
        &mut k9,
        "9 UID COPY 1 \"Accounts/Work/Projects/2026\"",
        &["[COPYUID "],
    )
    .await;
    ok(&mut k9, "10 UID STORE 1 +FLAGS.SILENT (\\Deleted)", &[]).await;
    ok(&mut k9, "11 UID EXPUNGE 1", &["* VANISHED 1"]).await;
    let draft = append(
        &mut k9,
        "12 APPEND Drafts (\\Seen \\Draft)",
        &invented("From K-9", "k9-draft"),
    )
    .await;
    assert!(status(&draft).starts_with("12 OK [APPENDUID "), "{draft}");

    let mut other = instance.connect().await;
    other.login(EDITOR).await;
    other.run("1 SELECT \"Accounts/Work/Projects/2026\"").await;
    other.send("2 IDLE").await;
    assert_eq!(other.line().await, "+ idling\r\n");
    k9.send("13 IDLE").await;
    assert_eq!(k9.line().await, "+ idling\r\n");
    instance
        .world
        .driver
        .deliver(
            "Work",
            "Projects/2026",
            &invented("Into the project", "project-new"),
        )
        .await;
    instance.signal.wake();
    assert_eq!(other.line().await, "* 3 EXISTS\r\n");
    k9.send("DONE").await;
    let answer = k9.until("13").await;
    assert!(!answer.contains("EXISTS"), "INBOX did not change: {answer}");
}
