use std::collections::HashMap;

use holtmail_category::testing::FakeCategorizer;
use holtmail_category::{CategoryVote, Topic};
use holtmail_category_signals::SignalsCategorizer;
use holtmail_core::{Address, MessageId, Timestamp};
use serde_json::{Value, json};

use super::*;

const TRUSTED: &str = "mx.example.org";
const UNSUBSCRIBE: &[&str] = &["from", "list-unsubscribe", "list-unsubscribe-post"];

#[derive(Clone)]
struct Env {
    archive: ArchiveId,
    imap: AccountId,
    import: AccountId,
    smtp: AccountId,
    accounts: Vec<Account>,
    stored: BTreeMap<AccountId, Vec<String>>,
    own: Vec<String>,
}

fn account(archive: ArchiveId, kind: AccountKind, name: &str, address: Option<&str>) -> Account {
    Account {
        id: AccountId::new(),
        archive_id: archive,
        kind,
        name: name.into(),
        address: address.map(str::to_owned),
        settings: Value::Null,
        sealed_secret: None,
        enabled: true,
        created_at: Timestamp::from_secs(0),
    }
}

fn env() -> Env {
    let archive = ArchiveId::new();
    let accounts = vec![
        account(
            archive,
            AccountKind::Imap,
            "Mail",
            Some(" Ada@Example.org "),
        ),
        account(archive, AccountKind::Import, "Import", None),
        account(archive, AccountKind::Import, "Received by SMTP", None),
    ];
    let stored = BTreeMap::from([(accounts[0].id, vec![" MX.Example.ORG ".to_owned()])]);
    Env {
        archive,
        imap: accounts[0].id,
        import: accounts[1].id,
        smtp: accounts[2].id,
        accounts,
        stored,
        own: vec!["shop@alias.example.org".into()],
    }
}

fn config() -> DeriveConfig {
    DeriveConfig {
        smtp_account_name: "Received by SMTP".into(),
        ..DeriveConfig::default()
    }
}

fn signals_only(config: DeriveConfig) -> Deriver {
    Deriver::new(vec![Arc::new(SignalsCategorizer::new())], config)
}

fn ctx(deriver: &Deriver, env: &Env) -> DeriveContext {
    deriver.context_from(env.archive, &env.accounts, &env.stored, &env.own)
}

fn pass(d: Option<&str>, i: Option<&str>, s: Option<&str>, b: Option<&str>) -> Value {
    json!({"d": d, "i": i, "s": s, "b": b})
}

fn pass_d(d: &str) -> Value {
    pass(Some(d), None, Some("s1"), Some("AbC12345"))
}

fn result(pos: u64, authserv: &str, dkim: Vec<Value>) -> Value {
    json!({"pos": pos, "authserv": authserv, "dkim": dkim})
}

fn sig(d: &str, s: &str, b: &str, h: &[&str], l: bool) -> Value {
    let h: serde_json::Map<String, Value> = h.iter().map(|n| (n.to_string(), json!(1))).collect();
    json!({"d": d, "s": s, "b": b, "h": h, "l": l})
}

fn good_sig() -> Value {
    sig("news.example.com", "s1", "AbC12345", UNSUBSCRIBE, false)
}

fn facts(results: Vec<Value>, signatures: Vec<Value>) -> Value {
    json!({
        "v": 2,
        "results": results,
        "signatures": signatures,
        "fields": {"from": 1, "list-unsubscribe": 1, "list-unsubscribe-post": 1},
        "sig_cap_hit": false,
        "from_domain": "news.example.com",
        "from_addresses": 1
    })
}

fn aligned_facts() -> Value {
    facts(
        vec![result(3, TRUSTED, vec![pass_d("news.example.com")])],
        vec![good_sig()],
    )
}

fn row(env: &Env, auth: &Value) -> DeriveRow {
    DeriveRow {
        message: MessageId::new(),
        archive: env.archive,
        from_address: Some("news@news.example.com".into()),
        signals: 0,
        kinds: 0,
        auth: auth.to_string(),
        items: Items::default().to_json(),
        alias: None,
        accounts: vec![env.imap],
    }
}

fn derive(auth: &Value) -> CategoryWrite {
    let env = env();
    let deriver = signals_only(config());
    deriver.derive(&ctx(&deriver, &env), &row(&env, auth))
}

fn trust(auth: &Value) -> TrustState {
    derive(auth).trust
}

fn aligned(domain: &str) -> TrustState {
    TrustState::Aligned {
        domain: domain.into(),
    }
}

fn alias(signed: bool) -> MessageAlias {
    MessageAlias {
        provider: "simplelogin".into(),
        alias: "shop@alias.example.org".into(),
        sender: Some(Address {
            name: None,
            email: "Grace@Shop.Example.COM".into(),
        }),
        reverse_alias: Some("ra@sl.example.org".into()),
        signed,
    }
}

#[test]
fn aligned_and_covered() {
    let write = derive(&aligned_facts());
    assert_eq!(write.trust, aligned("news.example.com"));
    assert_eq!(write.trust_authserv.as_deref(), Some(TRUSTED));
    assert!(write.one_click_ok);
    assert_eq!(
        write.sender_address.as_deref(),
        Some("news@news.example.com")
    );
    assert_eq!(write.sender_domain.as_deref(), Some("example.com"));
    assert!(!write.own);
}

#[test]
fn an_alias_hit_is_decided_first_and_never_aligned() {
    let env = env();
    let deriver = signals_only(config());
    let ctx = ctx(&deriver, &env);
    let mut row = row(&env, &aligned_facts());
    row.alias = Some(alias(true));
    let write = deriver.derive(&ctx, &row);
    assert_eq!(
        write.trust,
        TrustState::Relayed {
            provider: "simplelogin".into()
        }
    );
    assert_eq!(write.trust_authserv, None);
    assert!(!write.one_click_ok);
    assert_eq!(
        write.sender_address.as_deref(),
        Some("grace@shop.example.com")
    );
    assert_eq!(write.sender_domain.as_deref(), Some("example.com"));

    row.alias = Some(alias(false));
    let write = deriver.derive(&ctx, &row);
    assert_eq!(write.trust, TrustState::Unverified);
    assert!(!write.one_click_ok);
    assert_eq!(
        write.sender_address.as_deref(),
        Some("grace@shop.example.com")
    );

    row.alias = Some(MessageAlias {
        sender: None,
        ..alias(false)
    });
    assert_eq!(
        deriver.derive(&ctx, &row).sender_address.as_deref(),
        Some("news@news.example.com")
    );
}

#[test]
fn unknown_without_a_trusted_result() {
    let env = env();
    let deriver = signals_only(config());
    let ctx = ctx(&deriver, &env);
    let mut imported = row(&env, &aligned_facts());
    imported.accounts = vec![env.import];
    assert_eq!(deriver.derive(&ctx, &imported).trust, TrustState::Unknown);
    imported.accounts = vec![env.smtp];
    assert_eq!(deriver.derive(&ctx, &imported).trust, TrustState::Unknown);
    imported.accounts = vec![];
    assert_eq!(deriver.derive(&ctx, &imported).trust, TrustState::Unknown);

    let other = facts(
        vec![result(
            0,
            "mx.example.net",
            vec![pass_d("news.example.com")],
        )],
        vec![good_sig()],
    );
    assert_eq!(trust(&other), TrustState::Unknown);
    assert_eq!(trust(&facts(vec![], vec![good_sig()])), TrustState::Unknown);
    for broken in [
        json!("not facts"),
        json!({"v": 2}),
        json!({"v": 1, "results": "x"}),
    ] {
        let write = derive(&broken);
        assert_eq!(write.trust, TrustState::Unknown);
        assert!(!write.one_click_ok);
    }
    let write = deriver.derive(
        &ctx,
        &DeriveRow {
            auth: "{".into(),
            items: "[".into(),
            ..row(&env, &json!({}))
        },
    );
    assert_eq!(write.trust, TrustState::Unknown);
}

#[test]
fn import_and_smtp_accounts_use_the_configured_lists() {
    let env = env();
    let deriver = signals_only(DeriveConfig {
        import_authserv_ids: vec!["import.example.org".into()],
        smtp_authserv_ids: vec!["MX.example.org".into()],
        ..config()
    });
    let ctx = ctx(&deriver, &env);
    assert_eq!(ctx.trusted_authserv_ids(env.smtp), [TRUSTED]);
    assert_eq!(ctx.trusted_authserv_ids(env.import), ["import.example.org"]);
    assert_eq!(ctx.trusted_authserv_ids(env.imap), [TRUSTED]);
    let mut smtp = row(&env, &aligned_facts());
    smtp.accounts = vec![env.smtp];
    assert_eq!(
        deriver.derive(&ctx, &smtp).trust,
        aligned("news.example.com")
    );
    smtp.accounts = vec![env.import];
    assert_eq!(deriver.derive(&ctx, &smtp).trust, TrustState::Unknown);
    smtp.accounts = vec![env.import, env.imap];
    assert_eq!(
        deriver.derive(&ctx, &smtp).trust,
        aligned("news.example.com")
    );
}

#[test]
fn only_the_topmost_trusted_result_is_read() {
    let forged_above = facts(
        vec![
            result(0, "mx.example.net", vec![pass_d("bank.example.com")]),
            result(4, TRUSTED, vec![pass_d("news.example.com")]),
        ],
        vec![good_sig()],
    );
    let write = derive(&forged_above);
    assert_eq!(write.trust, aligned("news.example.com"));
    assert_eq!(write.trust_authserv.as_deref(), Some(TRUSTED));

    let failing_top = facts(
        vec![
            result(9, TRUSTED, vec![pass_d("news.example.com")]),
            result(2, TRUSTED, vec![]),
        ],
        vec![good_sig()],
    );
    assert_eq!(trust(&failing_top), TrustState::Unverified);
}

#[test]
fn exactly_one_from_field() {
    let mut two = aligned_facts();
    two["fields"]["from"] = json!(2);
    assert_eq!(trust(&two), TrustState::Unverified);
    let mut none = aligned_facts();
    none["fields"]["from"] = json!(0);
    assert_eq!(trust(&none), TrustState::Unverified);
    let mut no_domain = aligned_facts();
    no_domain["from_domain"] = Value::Null;
    assert_eq!(trust(&no_domain), TrustState::Unverified);
    let mut two_addresses = aligned_facts();
    two_addresses["from_addresses"] = json!(2);
    assert_eq!(trust(&two_addresses), TrustState::Unverified);
    let mut no_count = aligned_facts();
    no_count.as_object_mut().unwrap().remove("from_addresses");
    assert_eq!(trust(&no_count), TrustState::Unverified);
}

#[test]
fn header_i_stands_in_for_header_d() {
    let by_i = facts(
        vec![result(
            0,
            TRUSTED,
            vec![pass(None, Some("@news.example.com"), None, None)],
        )],
        vec![good_sig()],
    );
    assert_eq!(trust(&by_i), aligned("news.example.com"));
}

#[test]
fn alignment_is_by_registrable_domain() {
    let parent = facts(
        vec![result(0, TRUSTED, vec![pass_d("Example.COM")])],
        vec![sig("example.com", "s1", "AbC12345", UNSUBSCRIBE, false)],
    );
    let write = derive(&parent);
    assert_eq!(write.trust, aligned("example.com"));
    assert!(write.one_click_ok);

    let other = facts(
        vec![result(0, TRUSTED, vec![pass_d("esp.example.net")])],
        vec![sig("esp.example.net", "s1", "AbC12345", UNSUBSCRIBE, false)],
    );
    assert_eq!(trust(&other), TrustState::Unverified);
}

#[test]
fn a_matching_signature_is_required() {
    let none = facts(
        vec![result(0, TRUSTED, vec![pass_d("news.example.com")])],
        vec![],
    );
    assert_eq!(trust(&none), TrustState::Unverified);
    let other_selector = facts(
        vec![result(0, TRUSTED, vec![pass_d("news.example.com")])],
        vec![sig(
            "news.example.com",
            "s2",
            "AbC12345",
            UNSUBSCRIBE,
            false,
        )],
    );
    assert_eq!(trust(&other_selector), TrustState::Unverified);
    let other_b = facts(
        vec![result(0, TRUSTED, vec![pass_d("news.example.com")])],
        vec![sig(
            "news.example.com",
            "s1",
            "Zzz12345",
            UNSUBSCRIBE,
            false,
        )],
    );
    assert_eq!(trust(&other_b), TrustState::Unverified);
    let case = facts(
        vec![result(0, TRUSTED, vec![pass_d("NEWS.example.com")])],
        vec![sig(
            "news.EXAMPLE.com",
            "S1",
            "AbC12345",
            UNSUBSCRIBE,
            false,
        )],
    );
    assert_eq!(trust(&case), aligned("news.example.com"));
}

#[test]
fn l_tag_and_signature_cap_give_unverified() {
    let l = facts(
        vec![result(0, TRUSTED, vec![pass_d("news.example.com")])],
        vec![sig("news.example.com", "s1", "AbC12345", UNSUBSCRIBE, true)],
    );
    assert_eq!(trust(&l), TrustState::Unverified);
    let one_of_two = facts(
        vec![result(
            0,
            TRUSTED,
            vec![pass(Some("news.example.com"), None, None, None)],
        )],
        vec![
            good_sig(),
            sig("news.example.com", "s2", "Other123", UNSUBSCRIBE, true),
        ],
    );
    assert_eq!(trust(&one_of_two), TrustState::Unverified);

    let mut capped = aligned_facts();
    capped["sig_cap_hit"] = json!(true);
    let write = derive(&capped);
    assert_eq!(write.trust, TrustState::Unverified);
    assert!(!write.one_click_ok);
}

#[test]
fn coverage_needs_both_fields_signed_once_each() {
    let not_post = facts(
        vec![result(0, TRUSTED, vec![pass_d("news.example.com")])],
        vec![sig(
            "news.example.com",
            "s1",
            "AbC12345",
            &["from", "list-unsubscribe"],
            false,
        )],
    );
    let write = derive(&not_post);
    assert_eq!(write.trust, aligned("news.example.com"));
    assert!(!write.one_click_ok);

    for (field, count) in [
        ("list-unsubscribe", 2),
        ("list-unsubscribe-post", 2),
        ("list-unsubscribe", 0),
        ("list-unsubscribe-post", 0),
    ] {
        let mut fields = aligned_facts();
        fields["fields"][field] = json!(count);
        let write = derive(&fields);
        assert_eq!(write.trust, aligned("news.example.com"), "{field} {count}");
        assert!(!write.one_click_ok, "{field} {count}");
    }

    let narrowed = facts(
        vec![result(0, TRUSTED, vec![pass_d("news.example.com")])],
        vec![
            good_sig(),
            sig("news.example.com", "s2", "Other123", &["from"], false),
        ],
    );
    assert!(derive(&narrowed).one_click_ok);
    let unnarrowed = facts(
        vec![result(
            0,
            TRUSTED,
            vec![pass(Some("news.example.com"), None, None, None)],
        )],
        vec![
            good_sig(),
            sig("news.example.com", "s2", "Other123", &["from"], false),
        ],
    );
    let write = derive(&unnarrowed);
    assert_eq!(write.trust, aligned("news.example.com"));
    assert!(!write.one_click_ok);

    let parent_signs_only = facts(
        vec![result(
            0,
            TRUSTED,
            vec![pass_d("news.example.com"), pass_d("example.com")],
        )],
        vec![
            good_sig(),
            sig("example.com", "s1", "AbC12345", &["from"], false),
        ],
    );
    let write = derive(&parent_signs_only);
    assert_eq!(write.trust, aligned("news.example.com"));
    assert!(
        !write.one_click_ok,
        "every candidate of the result must sign both fields"
    );
}

fn vote(kind: Option<MessageKind>, source: CategorySource, topics: &[Topic]) -> CategoryVote {
    CategoryVote {
        kind,
        kind_source: source,
        topics: topics.iter().copied().collect(),
        source,
    }
}

fn domains(votes: &[(&str, CategoryVote)]) -> Arc<FakeCategorizer> {
    Arc::new(FakeCategorizer::new(
        "domains",
        "t1",
        votes
            .iter()
            .map(|(domain, vote)| (domain.to_string(), vote.clone()))
            .collect::<HashMap<_, _>>(),
    ))
}

fn with_domains(fake: Arc<FakeCategorizer>, config: DeriveConfig) -> Deriver {
    Deriver::new(
        vec![Arc::new(SignalsCategorizer::new()), fake],
        DeriveConfig {
            sources: vec!["signals".into(), "domains".into()],
            ..config
        },
    )
}

#[test]
fn topics_are_the_union_and_the_kind_follows_rank() {
    let env = env();
    let fake = domains(&[(
        "example.com",
        vote(
            Some(MessageKind::Bulk),
            CategorySource::Domain,
            &[Topic::Travel, Topic::Purchases],
        ),
    )]);
    let deriver = with_domains(fake, config());
    let ctx = ctx(&deriver, &env);
    let mut row = row(&env, &aligned_facts());
    row.kinds = KindTags::EMPTY
        .with(holtmail_category::KindTag::Order)
        .bits();
    let write = deriver.derive(&ctx, &row);
    assert_eq!(write.derived_kind, MessageKind::Bulk);
    assert_eq!(write.derived_kind_source, CategorySource::Domain);
    assert_eq!(
        write.derived_topics,
        [Topic::Purchases, Topic::Travel].into_iter().collect()
    );
    assert_eq!(
        write.derived_sources,
        BTreeMap::from([
            (Topic::Purchases, CategorySource::Content),
            (Topic::Travel, CategorySource::Domain),
        ])
    );

    row.signals = Signals::EMPTY.with(Signal::Structured).bits();
    let write = deriver.derive(&ctx, &row);
    assert_eq!(write.derived_kind, MessageKind::Notification);
    assert_eq!(write.derived_kind_source, CategorySource::Content);

    row.from_address = Some("Ada@Example.org".into());
    let write = deriver.derive(&ctx, &row);
    assert!(write.own);
    assert_eq!(write.derived_kind, MessageKind::Personal);
    assert_eq!(write.derived_kind_source, CategorySource::Headers);
}

#[test]
fn a_source_not_enabled_never_votes() {
    let env = env();
    let fake = domains(&[(
        "example.com",
        vote(
            Some(MessageKind::Bulk),
            CategorySource::Domain,
            &[Topic::Travel],
        ),
    )]);
    let deriver = Deriver::new(vec![Arc::new(SignalsCategorizer::new()), fake], config());
    let write = deriver.derive(&ctx(&deriver, &env), &row(&env, &aligned_facts()));
    assert_eq!(write.derived_kind, MessageKind::Personal);
    assert_eq!(write.derived_kind_source, CategorySource::Headers);
    assert!(write.derived_topics.is_empty());

    let nothing = Deriver::new(vec![], config());
    let write = nothing.derive(&ctx(&nothing, &env), &row(&env, &aligned_facts()));
    assert_eq!(write.derived_kind, MessageKind::Personal);
    assert_eq!(write.derived_kind_source, CategorySource::Headers);
}

#[test]
fn sensitive_topics_need_alignment_and_health_follows_the_flag() {
    let env = env();
    let sensitive = vote(
        None,
        CategorySource::Domain,
        &[Topic::Finance, Topic::Health, Topic::Travel],
    );
    let fake = domains(&[("example.com", sensitive)]);
    let on = with_domains(fake.clone(), config());
    let off = with_domains(
        fake,
        DeriveConfig {
            health: false,
            ..config()
        },
    );

    let aligned_row = row(&env, &aligned_facts());
    let write = on.derive(&ctx(&on, &env), &aligned_row);
    assert_eq!(
        write.derived_topics,
        [Topic::Finance, Topic::Health, Topic::Travel]
            .into_iter()
            .collect()
    );
    assert_eq!(write.derived_sources.len(), 3);
    let write = off.derive(&ctx(&off, &env), &aligned_row);
    assert_eq!(
        write.derived_topics,
        [Topic::Finance, Topic::Travel].into_iter().collect()
    );
    assert!(!write.derived_sources.contains_key(&Topic::Health));

    let mut unverified = row(&env, &facts(vec![result(0, TRUSTED, vec![])], vec![]));
    for alias in [None, Some(alias(true))] {
        unverified.alias = alias.map(|a| MessageAlias { sender: None, ..a });
        let write = on.derive(&ctx(&on, &env), &unverified);
        assert!(!write.trust.is_aligned());
        assert_eq!(write.derived_topics, Topic::Travel.into());
        assert_eq!(
            write.derived_sources,
            BTreeMap::from([(Topic::Travel, CategorySource::Domain)])
        );
    }

    let mut invoice = row(&env, &json!({}));
    invoice.kinds = KindTags::EMPTY
        .with(holtmail_category::KindTag::Invoice)
        .bits();
    let write = on.derive(&ctx(&on, &env), &invoice);
    assert_eq!(write.trust, TrustState::Unknown);
    assert_eq!(write.derived_topics, Topic::Travel.into());
}

#[test]
fn derive_fp_follows_every_input() {
    let env = env();
    let fake = domains(&[]);
    let base = with_domains(fake.clone(), config());
    let fp = |deriver: &Deriver, env: &Env| ctx(deriver, env).derive_fp().to_owned();
    let start = fp(&base, &env);
    assert_eq!(start.len(), 64);
    assert_eq!(fp(&base, &env), start, "equal inputs, equal fingerprint");
    assert_eq!(
        fp(&with_domains(fake.clone(), config()), &env),
        start,
        "a new deriver over the same inputs"
    );

    let mut reordered = env;
    reordered.own = vec!["SHOP@alias.example.org".into(), "ada@example.org".into()];
    reordered
        .stored
        .insert(reordered.imap, vec![TRUSTED.into(), TRUSTED.into()]);
    assert_eq!(fp(&base, &reordered), start, "order, case and repeats");
    let env = reordered;

    let mut changes = vec![fp(
        &with_domains(
            fake.clone(),
            DeriveConfig {
                health: false,
                ..config()
            },
        ),
        &env,
    )];
    changes.push(fp(&signals_only(config()), &env));
    changes.push(fp(
        &with_domains(
            fake.clone(),
            DeriveConfig {
                import_authserv_ids: vec!["import.example.org".into()],
                ..config()
            },
        ),
        &env,
    ));
    changes.push(fp(
        &with_domains(
            fake.clone(),
            DeriveConfig {
                smtp_authserv_ids: vec![TRUSTED.into()],
                ..config()
            },
        ),
        &env,
    ));
    let mut lists = env.clone();
    lists.stored = BTreeMap::from([(env.imap, vec!["mx2.example.org".to_owned()])]);
    changes.push(fp(&base, &lists));
    lists.stored = env.stored.clone();
    lists.own.push("grace@example.com".into());
    changes.push(fp(&base, &lists));
    lists.own = env.own.clone();
    lists.accounts[0].address = Some("ada2@example.org".into());
    changes.push(fp(&base, &lists));
    fake.swap("t2", HashMap::new());
    changes.push(fp(&base, &env));

    let distinct: BTreeSet<&String> = changes.iter().chain([&start]).collect();
    assert_eq!(distinct.len(), changes.len() + 1, "{changes:?}");
}

#[test]
fn a_swapped_table_is_noticed_and_old_contexts_keep_theirs() {
    let env = env();
    let fake = domains(&[(
        "example.com",
        vote(None, CategorySource::Domain, &[Topic::Travel]),
    )]);
    let deriver = with_domains(fake.clone(), config());
    let before = ctx(&deriver, &env);
    assert!(!deriver.changed(&before));
    fake.swap(
        "t2",
        HashMap::from([(
            "example.com".to_owned(),
            vote(None, CategorySource::Domain, &[Topic::Jobs]),
        )]),
    );
    assert!(deriver.changed(&before));
    let row = row(&env, &aligned_facts());
    assert_eq!(
        deriver.derive(&before, &row).derived_topics,
        Topic::Travel.into()
    );
    let after = ctx(&deriver, &env);
    assert!(!deriver.changed(&after));
    assert_ne!(after.derive_fp(), before.derive_fp());
    let write = deriver.derive(&after, &row);
    assert_eq!(write.derived_topics, Topic::Jobs.into());
    assert_eq!(write.derive_fp, after.derive_fp());
}

#[test]
fn derive_is_deterministic() {
    let env = env();
    let deriver = signals_only(config());
    let ctx = ctx(&deriver, &env);
    let row = row(&env, &aligned_facts());
    assert_eq!(deriver.derive(&ctx, &row), deriver.derive(&ctx, &row));
}

fn with_top(authserv: &str) -> String {
    facts(vec![result(1, authserv, vec![])], vec![]).to_string()
}

#[test]
fn suggestion_thresholds() {
    let many = |good: usize, bad: usize| -> Vec<String> {
        let mut out = vec![with_top("MX.example.org"); good];
        out.extend(vec![with_top("other.example.net"); bad]);
        out
    };
    assert_eq!(suggest_authserv(&many(49, 0)), None);
    assert_eq!(
        suggest_authserv(&many(50, 0)).as_deref(),
        Some("mx.example.org")
    );
    assert_eq!(suggest_authserv(&many(94, 6)), None);
    assert_eq!(
        suggest_authserv(&many(95, 5)).as_deref(),
        Some("mx.example.org")
    );

    let mut padded = many(49, 0);
    padded.extend(vec![facts(vec![], vec![]).to_string(); 10]);
    padded.push("not json".into());
    assert_eq!(suggest_authserv(&padded), None, "facts without a result");

    let mut window = many(190, 10);
    window.extend(vec![with_top("late.example.net"); 300]);
    assert_eq!(
        suggest_authserv(&window).as_deref(),
        Some("mx.example.org"),
        "only the first 200 count"
    );
    let mut window = many(189, 11);
    window.extend(many(300, 0));
    assert_eq!(suggest_authserv(&window), None);

    let lower_first = facts(
        vec![
            result(7, "lower.example.net", vec![]),
            result(2, "mx.example.org", vec![]),
        ],
        vec![],
    )
    .to_string();
    assert_eq!(
        suggest_authserv(&vec![lower_first; 60]).as_deref(),
        Some("mx.example.org")
    );
}

#[test]
fn auth_summary_reads_leniently() {
    let mut auth = facts(
        vec![
            result(9, "b.example.org", vec![]),
            result(1, "a.example.org", vec![]),
        ],
        vec![],
    );
    auth["fields"]["list-unsubscribe"] = json!(2);
    assert_eq!(
        auth_summary(&auth.to_string()),
        AuthSummary {
            authserv_ids: vec!["a.example.org".into(), "b.example.org".into()],
            from_fields: 1,
            unsubscribe_fields: 2,
            unsubscribe_post_fields: 1,
        }
    );
    assert_eq!(auth_summary("nope"), AuthSummary::default());
    assert_eq!(auth_summary(r#"{"v":7}"#), AuthSummary::default());
}

#[test]
fn override_keys() {
    use OverrideScope::{Address as A, Domain as D};
    let cases: &[(OverrideScope, &str, Option<&str>)] = &[
        (A, "  Ada+News@Example.COM. ", Some("ada+news@example.com")),
        (A, "ada@bücher.example", Some("ada@xn--bcher-kva.example")),
        (A, "ada@example", None),
        (A, "ada@@example.com", None),
        (A, "ada@x@example.com", None),
        (A, "@example.com", None),
        (A, "ada@", None),
        (A, "example.com", None),
        (A, "ada@[192.0.2.1]", None),
        (A, "a da@example.com", None),
        (A, "ada@example..com", None),
        (A, "", None),
        (D, "News.Example.COM.", Some("example.com")),
        (D, "ada@news.example.co.uk", Some("example.co.uk")),
        (D, "bücher.example", Some("xn--bcher-kva.example")),
        (D, "co.uk", None),
        (D, "localhost", None),
        (D, "192.0.2.1", None),
        (D, "@example.com", None),
        (D, "", None),
    ];
    for (scope, raw, expected) in cases {
        assert_eq!(
            override_key(*scope, raw).as_deref(),
            *expected,
            "{scope:?} {raw:?}"
        );
    }
    assert_eq!(override_key(A, &format!("a@{}.com", "x".repeat(400))), None);
}

#[test]
fn own_addresses_are_normalised() {
    let env = env();
    let deriver = signals_only(config());
    let ctx = ctx(&deriver, &env);
    assert!(ctx.is_own("ADA@example.org"));
    assert!(ctx.is_own("shop@alias.example.org"));
    assert!(!ctx.is_own("grace@example.com"));
    assert_eq!(ctx.archive(), env.archive);
}
