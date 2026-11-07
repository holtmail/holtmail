//! Stage A then stage B over hand-written messages: trust from the stored auth facts, the trust
//! gate on derived topics, and on override topics once the store applies them.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use holtmail_blob::BlobId;
use holtmail_category::{MessageKind, Topic, TopicSet, TrustState};
use holtmail_category_signals::SignalsCategorizer;
use holtmail_core::{AccountId, ArchiveId, MessageId, Timestamp, UserId};
use holtmail_insights::{DeriveConfig, Deriver, Extracted};
use holtmail_store::{
    Account, AccountKind, CategoryOverride, CategoryOverrideId, DeriveRow, NewAccount, NewMessage,
    OverrideScope, Store,
};
use holtmail_store_sqlite::SqliteStore;

const SMTP: &str = "Received by SMTP";
const SIGNATURE: &str = "DKIM-Signature: v=1; a=rsa-sha256; d=bank.example.com; s=s1; h=from:subject; bh=x; b=ZmFrZWZhSignedForReal\r\n";

fn read(path: &str) -> Vec<u8> {
    std::fs::read(format!("{}/{path}", env!("CARGO_MANIFEST_DIR")))
        .unwrap_or_else(|_| panic!("{path} is missing"))
}

fn finance(name: &str) -> Vec<u8> {
    read(&format!(
        "../category-signals/tests/fixtures/finance-{name}.eml"
    ))
}

fn signed(raw: &[u8]) -> Vec<u8> {
    let mut out = SIGNATURE.as_bytes().to_vec();
    out.extend_from_slice(raw);
    out
}

fn extract(raw: &[u8]) -> Extracted {
    common::runner().extract(&holtmail_mime::parse(raw).unwrap())
}

fn account(archive: ArchiveId, kind: AccountKind, name: &str) -> Account {
    Account {
        id: AccountId::new(),
        archive_id: archive,
        kind,
        name: name.into(),
        address: Some("ada@example.org".into()),
        settings: serde_json::Value::Null,
        sealed_secret: None,
        enabled: true,
        created_at: Timestamp::from_secs(0),
    }
}

fn row(archive: ArchiveId, account: AccountId, extracted: &Extracted) -> DeriveRow {
    DeriveRow {
        message: MessageId::new(),
        archive,
        from_address: extracted.from_address.clone(),
        signals: extracted.signals.bits(),
        kinds: extracted.kinds.bits(),
        auth: extracted.auth.clone(),
        items: extracted.items.to_json(),
        alias: None,
        accounts: vec![account],
    }
}

fn deriver(config: DeriveConfig) -> Deriver {
    Deriver::new(vec![Arc::new(SignalsCategorizer::new())], config)
}

#[test]
fn forged_results_on_the_smtp_path_are_unknown_without_a_trust_list() {
    let archive = ArchiveId::new();
    let smtp = account(archive, AccountKind::Import, SMTP);
    let extracted = extract(&read("tests/fixtures/smtp-forged-ar-no-trust.eml"));
    let row = row(archive, smtp.id, &extracted);
    let config = DeriveConfig {
        smtp_account_name: SMTP.into(),
        ..DeriveConfig::default()
    };

    let untrusting = deriver(config.clone());
    let ctx = untrusting.context_from(archive, std::slice::from_ref(&smtp), &BTreeMap::new(), &[]);
    assert!(ctx.trusted_authserv_ids(smtp.id).is_empty());
    let write = untrusting.derive(&ctx, &row);
    assert_eq!(write.trust, TrustState::Unknown);
    assert_eq!(write.trust_authserv, None);
    assert!(!write.one_click_ok);
    assert_eq!(
        write.sender_address.as_deref(),
        Some("alerts@bank.example.com")
    );
    assert_eq!(write.sender_domain.as_deref(), Some("example.com"));

    let trusting = deriver(DeriveConfig {
        smtp_authserv_ids: vec!["mx.example.org".into()],
        ..config
    });
    let ctx = trusting.context_from(archive, &[smtp], &BTreeMap::new(), &[]);
    assert_eq!(
        trusting.derive(&ctx, &row).trust,
        TrustState::Unverified,
        "the relay's signature does not match the claimed pass"
    );
}

#[test]
fn spoofed_finance_is_gated_and_aligned_finance_kept() {
    let archive = ArchiveId::new();
    let imap = account(archive, AccountKind::Imap, "Mail");
    let stored = BTreeMap::from([(imap.id, vec!["mx.example.org".to_owned()])]);
    let deriver = deriver(DeriveConfig::default());
    let ctx = deriver.context_from(archive, std::slice::from_ref(&imap), &stored, &[]);

    let spoofed = deriver.derive(
        &ctx,
        &row(archive, imap.id, &extract(&finance("spoofed-unverified"))),
    );
    assert_eq!(spoofed.trust, TrustState::Unverified);
    assert_eq!(spoofed.trust_authserv.as_deref(), Some("mx.example.org"));
    assert_eq!(spoofed.derived_kind, MessageKind::Notification);
    assert!(!spoofed.derived_topics.contains(Topic::Finance));
    assert!(spoofed.derived_sources.is_empty());

    let real = deriver.derive(
        &ctx,
        &row(
            archive,
            imap.id,
            &extract(&signed(&finance("spoofed-unverified"))),
        ),
    );
    assert_eq!(
        real.trust,
        TrustState::Aligned {
            domain: "bank.example.com".into()
        }
    );
    assert_eq!(real.derived_topics, Topic::Finance.into());
}

async fn store_message(
    store: &dyn Store,
    archive: ArchiveId,
    folder: holtmail_core::FolderId,
    uid: u64,
    raw: &[u8],
) -> MessageId {
    let parsed = holtmail_mime::parse(raw).unwrap();
    let message = store
        .insert_message(NewMessage {
            archive_id: archive,
            blob_id: BlobId::of(raw),
            size: raw.len() as u64,
            rfc_message_id: parsed.message_id.clone(),
            parent_ids: Vec::new(),
            subject: parsed.subject.clone(),
            from: parsed.from.clone(),
            to: parsed.to.clone(),
            cc: Vec::new(),
            bcc: Vec::new(),
            sent_at: parsed.sent_at.map(Timestamp::from_secs),
            received_at: None,
            preview: String::new(),
            attachments: Vec::new(),
        })
        .await
        .unwrap()
        .message_id;
    store
        .record_location(message, folder, Some(uid), &[], Timestamp::now())
        .await
        .unwrap();
    let runner = common::runner();
    let row =
        runner
            .extract(&parsed)
            .into_row(message, archive, runner.extract_fp(), Timestamp::now());
    store.set_message_insights(message, &row).await.unwrap();
    message
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finance_override_applies_to_aligned_mail_only() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
    let archive = store.create_archive("Test").await.unwrap().id;
    let account = store
        .create_account(NewAccount {
            archive_id: archive,
            kind: AccountKind::Imap,
            name: "Mail".into(),
            address: Some("ada@example.org".into()),
            settings: Default::default(),
            sealed_secret: None,
            enabled: false,
        })
        .await
        .unwrap();
    store
        .set_trusted_authserv_ids(account.id, &["mx.example.org".into()])
        .await
        .unwrap();
    let inbox = store
        .upsert_folder(account.id, "INBOX", None)
        .await
        .unwrap()
        .id;
    let spoofed = store_message(&*store, archive, inbox, 1, &finance("override-spoofed")).await;
    let real = store_message(
        &*store,
        archive,
        inbox,
        2,
        &signed(&finance("override-spoofed")),
    )
    .await;

    let deriver = deriver(DeriveConfig::default());
    let ctx = deriver.context(&*store, archive).await.unwrap();
    let mut writes = Vec::new();
    for message in [spoofed, real] {
        let input = store.derive_input(message).await.unwrap().unwrap();
        assert_eq!(input.accounts, [account.id]);
        writes.push(deriver.derive(&ctx, &input));
    }
    assert_eq!(writes[0].trust, TrustState::Unverified);
    assert!(writes[1].trust.is_aligned());
    assert!(writes.iter().all(|w| w.derived_topics.is_empty()));
    store
        .set_message_categories(&writes, deriver.policy())
        .await
        .unwrap();
    assert_eq!(
        store
            .count_needing_derive(archive, ctx.derive_fp())
            .await
            .unwrap(),
        0
    );

    store
        .put_category_override(
            &CategoryOverride {
                id: CategoryOverrideId::new(),
                archive_id: archive,
                scope: OverrideScope::Domain,
                key: holtmail_insights::override_key(OverrideScope::Domain, "Bank.Example.COM")
                    .unwrap(),
                kind: None,
                topics: Some(Topic::Finance.into()),
                created_at: Timestamp::now(),
                created_by: UserId::new(),
            },
            deriver.policy(),
        )
        .await
        .unwrap();
    let topics = |message| {
        let store = store.clone();
        async move {
            store
                .insight_view(message)
                .await
                .unwrap()
                .unwrap()
                .categories
                .unwrap()
                .row
                .topics
        }
    };
    assert_eq!(topics(spoofed).await, TopicSet::EMPTY);
    assert_eq!(topics(real).await, Topic::Finance.into());
}
