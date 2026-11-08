use holtmail_alias::testing::FakeDirectory;
use holtmail_secret::SecretError;
use holtmail_store_sqlite::SqliteStore;

use super::*;

struct Plain;

impl SecretBox for Plain {
    fn seal(&self, context: &str, plaintext: &[u8]) -> Result<Vec<u8>, SecretError> {
        Ok([context.as_bytes(), b"|", plaintext].concat())
    }

    fn open(&self, context: &str, sealed: &[u8]) -> Result<Vec<u8>, SecretError> {
        sealed
            .strip_prefix(format!("{context}|").as_bytes())
            .map(<[u8]>::to_vec)
            .ok_or(SecretError::Invalid)
    }
}

struct Fixture {
    manager: AliasManager,
    store: Arc<dyn Store>,
    fake: Arc<FakeDirectory>,
    archive: ArchiveId,
    _dir: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open(dir.path().join("db")).await.unwrap());
    let archive = store.create_archive("Test").await.unwrap().id;
    let fake = Arc::new(FakeDirectory::new());
    let refusing = Arc::new(FakeDirectory::new());
    refusing.refuse_key().await;
    let shared = fake.clone();
    let connect: Connect = Arc::new(move |provider, url, key| {
        if provider != "fake" || !url.starts_with("https://") {
            return Err(AliasError::Invalid("no such service".into()));
        }
        Ok(if key == "good-key" {
            shared.clone() as Arc<dyn AliasDirectory>
        } else {
            refusing.clone() as Arc<dyn AliasDirectory>
        })
    });
    let providers = vec![
        ProviderInfo {
            id: "fake".into(),
            name: "Fake".into(),
            default_url: Some("https://alias.example".into()),
            capabilities: Some(fake.capabilities()),
            key_label: "API key".into(),
            key_help: "Settings".into(),
            detects: false,
        },
        ProviderInfo {
            id: "apple".into(),
            name: "Apple".into(),
            default_url: None,
            capabilities: None,
            key_label: String::new(),
            key_help: String::new(),
            detects: true,
        },
    ];
    Fixture {
        manager: AliasManager::new(store.clone(), Arc::new(Plain), providers, connect),
        store,
        fake,
        archive,
        _dir: dir,
    }
}

#[tokio::test]
async fn connects_lists_and_manages_aliases() {
    let f = fixture().await;
    f.fake.seed("old@alias.example").await;

    assert!(matches!(
        f.manager
            .add_service(f.archive, "fake", "", None, "bad-key")
            .await,
        Err(ManageError::Invalid(m)) if m == "the service refused the key"
    ));
    assert!(matches!(
        f.manager
            .add_service(f.archive, "apple", "", None, "x")
            .await,
        Err(ManageError::Invalid(_))
    ));
    assert!(f.store.alias_services(f.archive).await.unwrap().is_empty());

    let service = f
        .manager
        .add_service(f.archive, "fake", "  ", None, " good-key ")
        .await
        .unwrap();
    assert_eq!(service.name, "Fake");
    assert_eq!(service.base_url, "https://alias.example");
    assert!(service.synced_at.is_some());
    assert!(!String::from_utf8_lossy(&service.sealed_key).ends_with(" good-key "));
    let listed = f.store.aliases(f.archive).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].address, "old@alias.example");
    assert_eq!(listed[0].provider, "fake");

    let made = f
        .manager
        .create_alias(
            service.id,
            &NewAlias {
                prefix: Some("shop".into()),
                note: Some("for the shop".into()),
                ..NewAlias::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(made.address, "shop@alias.example");
    assert_eq!(made.mailboxes, ["owner@example.org"]);

    let off = f
        .manager
        .update_alias(
            f.archive,
            "shop@alias.example",
            &AliasChange {
                enabled: Some(false),
                mailbox_ids: Some(vec!["2".into()]),
                ..AliasChange::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(off.enabled, Some(false));
    assert_eq!(off.mailboxes, ["other@example.org"]);
    assert!(
        !f.fake
            .aliases()
            .await
            .unwrap()
            .iter()
            .find(|a| a.address == "shop@alias.example")
            .unwrap()
            .enabled
    );

    f.manager
        .delete_alias(f.archive, "shop@alias.example", true)
        .await
        .unwrap();
    assert_eq!(f.fake.aliases().await.unwrap().len(), 1);
    assert!(matches!(
        f.manager
            .delete_alias(f.archive, "shop@alias.example", true)
            .await,
        Err(ManageError::Invalid(_))
    ));

    f.fake.refuse_key().await;
    assert!(f.manager.sync_service(service.id).await.is_err());
    let failed = f
        .store
        .get_alias_service(service.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        failed.sync_error.as_deref(),
        Some("the service refused the key")
    );
    let (archives, failures) = f.manager.sync_all().await.unwrap();
    assert!(archives.is_empty());
    assert_eq!(failures, 1);
}

#[tokio::test]
async fn aliases_added_by_hand_keep_a_note() {
    let f = fixture().await;
    let manual = KnownAlias {
        address: "quiet.river@icloud.example.org".into(),
        provider: "apple".into(),
        service_id: None,
        remote_id: None,
        enabled: None,
        note: None,
        name: None,
        for_domain: None,
        mailboxes: Vec::new(),
        created_at: None,
        forwarded: None,
        blocked: None,
        replied: None,
    };
    f.store.put_alias(f.archive, &manual).await.unwrap();
    let noted = f
        .manager
        .update_alias(
            f.archive,
            "quiet.river@icloud.example.org",
            &AliasChange {
                note: Some("newsletters".into()),
                for_domain: Some("shop.example.com".into()),
                ..AliasChange::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(noted.note.as_deref(), Some("newsletters"));
    assert!(matches!(
        f.manager
            .update_alias(
                f.archive,
                "quiet.river@icloud.example.org",
                &AliasChange {
                    enabled: Some(false),
                    ..AliasChange::default()
                },
            )
            .await,
        Err(ManageError::Invalid(_))
    ));
    f.manager
        .delete_alias(f.archive, "quiet.river@icloud.example.org", true)
        .await
        .unwrap();
    assert!(f.store.aliases(f.archive).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_new_key_is_checked_before_it_is_kept() {
    let f = fixture().await;
    let service = f
        .manager
        .add_service(f.archive, "fake", "Mine", None, "good-key")
        .await
        .unwrap();
    assert!(
        f.manager
            .update_service(service.id, None, None, Some("bad-key"))
            .await
            .is_err()
    );
    let renamed = f
        .manager
        .update_service(service.id, Some("Renamed"), None, None)
        .await
        .unwrap();
    assert_eq!(renamed.name, "Renamed");
    assert_eq!(renamed.sealed_key, service.sealed_key);
    assert!(f.manager.sync_service(service.id).await.is_ok());
}

struct Plus;

impl AliasDetector for Plus {
    fn provider(&self) -> &str {
        "plus"
    }

    fn version(&self) -> String {
        "1".into()
    }

    fn detect(&self, _mail: &holtmail_alias::Mail) -> Option<holtmail_alias::AliasHit> {
        None
    }

    fn reply_strategy(&self) -> ReplyStrategy {
        ReplyStrategy::ReverseAlias
    }

    fn encode_reverse(&self, alias: &str, contact: &str) -> Option<String> {
        let (local, domain) = alias.split_once('@')?;
        Some(format!("{local}+{}@{domain}", contact.replace('@', "=")).to_ascii_lowercase())
    }
}

fn by_hand(address: &str, provider: &str) -> KnownAlias {
    KnownAlias {
        address: address.into(),
        provider: provider.into(),
        service_id: None,
        remote_id: None,
        enabled: None,
        note: None,
        name: None,
        for_domain: None,
        mailboxes: Vec::new(),
        created_at: None,
        forwarded: None,
        blocked: None,
        replied: None,
    }
}

#[tokio::test]
async fn reverse_aliases_come_from_the_service_or_the_detector() {
    let f = fixture().await;
    let manager = f.manager.clone().with_detectors(vec![Arc::new(Plus)]);
    f.fake.seed("shop@alias.example").await;
    manager
        .add_service(f.archive, "fake", "", None, "good-key")
        .await
        .unwrap();

    let first = manager
        .reverse_alias(f.archive, "Shop@alias.example", "Bob <bob@example.org>")
        .await
        .unwrap();
    assert!(first.starts_with("reverse") && first.ends_with("@alias.example"));
    assert_eq!(
        manager
            .reverse_alias(f.archive, "shop@alias.example", "bob@example.org")
            .await
            .unwrap(),
        first
    );
    assert_eq!(f.fake.reverse_aliases_made().await, 1);
    assert_eq!(
        f.store
            .reverse_alias_contact(f.archive, &first)
            .await
            .unwrap()
            .as_deref(),
        Some("bob@example.org"),
        "the pair is recorded for People"
    );
    assert!(
        manager
            .can_reverse(f.archive, "shop@alias.example")
            .await
            .unwrap()
    );
    assert_eq!(
        f.fake.reverse_aliases_made().await,
        1,
        "asking makes nothing"
    );

    f.store
        .put_alias(f.archive, &by_hand("news@plus.example.org", "plus"))
        .await
        .unwrap();
    assert_eq!(
        manager
            .reverse_alias(f.archive, "news@plus.example.org", "Bob <Bob@example.org>")
            .await
            .unwrap(),
        "news+bob=example.org@plus.example.org"
    );
    assert!(
        manager
            .can_reverse(f.archive, "News@plus.example.org")
            .await
            .unwrap()
    );
    assert!(
        !f.manager
            .can_reverse(f.archive, "news@plus.example.org")
            .await
            .unwrap()
    );

    f.store
        .put_alias(
            f.archive,
            &by_hand("quiet.river@icloud.example.org", "apple"),
        )
        .await
        .unwrap();
    assert!(matches!(
        manager
            .reverse_alias(
                f.archive,
                "quiet.river@icloud.example.org",
                "bob@example.org"
            )
            .await,
        Err(ManageError::Invalid(_))
    ));
    assert!(matches!(
        f.manager
            .reverse_alias(f.archive, "news@plus.example.org", "bob@example.org")
            .await,
        Err(ManageError::Invalid(_))
    ));
    assert!(matches!(
        manager
            .reverse_alias(f.archive, "nobody@plus.example.org", "bob@example.org")
            .await,
        Err(ManageError::Invalid(m)) if m.contains("not a known alias")
    ));
}

fn strategy(scope: StrategyScope, key: &str, strategy: &str) -> AliasStrategy {
    AliasStrategy {
        scope,
        key: key.into(),
        strategy: strategy.into(),
    }
}

#[test]
fn the_most_specific_override_wins() {
    let overrides = vec![
        strategy(StrategyScope::Service, "simplelogin", "direct"),
        strategy(StrategyScope::Domain, "sl.example.org", "reply_header"),
        strategy(StrategyScope::Alias, "shop@sl.example.org", "reverse_alias"),
        strategy(StrategyScope::Alias, "odd@sl.example.org", "teleport"),
    ];
    let pick = |alias: &str, provider: &str| {
        let e = effective_strategy(&overrides, alias, provider, ReplyStrategy::ReverseAlias);
        (e.strategy, e.source)
    };
    assert_eq!(
        pick("Shop@SL.example.org", "simplelogin"),
        (ReplyStrategy::ReverseAlias, StrategySource::Alias)
    );
    assert_eq!(
        pick("news@sl.example.org", "simplelogin"),
        (ReplyStrategy::ReplyHeader, StrategySource::Domain)
    );
    assert_eq!(
        pick("odd@sl.example.org", "simplelogin"),
        (ReplyStrategy::ReplyHeader, StrategySource::Domain),
        "an unknown strategy name is skipped"
    );
    assert_eq!(
        pick("news@other.example.org", "SimpleLogin"),
        (ReplyStrategy::Direct, StrategySource::Service)
    );
    assert_eq!(
        pick("news@other.example.org", "addy"),
        (ReplyStrategy::ReverseAlias, StrategySource::Default)
    );
    assert_eq!(StrategySource::Domain.as_str(), "domain");
}

#[tokio::test]
async fn the_stored_override_beats_the_detector() {
    let f = fixture().await;
    let manager = f.manager.clone().with_detectors(vec![Arc::new(Plus)]);
    assert_eq!(
        manager.default_strategy("plus"),
        ReplyStrategy::ReverseAlias
    );
    assert_eq!(manager.default_strategy("unknown"), ReplyStrategy::Direct);
    let before = manager
        .reply_strategy(f.archive, "news@plus.example.org", "plus")
        .await
        .unwrap();
    assert_eq!(before.source, StrategySource::Default);
    f.store
        .set_alias_strategy(
            f.archive,
            StrategyScope::Domain,
            "plus.example.org",
            Some("direct"),
        )
        .await
        .unwrap();
    let after = manager
        .reply_strategy(f.archive, "news@plus.example.org", "plus")
        .await
        .unwrap();
    assert_eq!(
        (after.strategy, after.source),
        (ReplyStrategy::Direct, StrategySource::Domain)
    );
}
