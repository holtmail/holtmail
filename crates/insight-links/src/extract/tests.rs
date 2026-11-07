use holtmail_insight::{HtmlView, InsightKind};

use super::*;
use crate::data::DATA;

fn anchor(index: u32, href: &str, text: &str) -> Anchor {
    Anchor {
        href: href.into(),
        text: text.into(),
        index,
        ..Anchor::default()
    }
}

fn button(index: u32, href: &str, text: &str) -> Anchor {
    Anchor {
        button_like: true,
        ..anchor(index, href, text)
    }
}

fn input(anchors: Vec<Anchor>) -> InsightInput {
    InsightInput {
        html: Some(HtmlView::new().with_anchors(anchors)),
        ..InsightInput::default()
    }
}

fn data() -> &'static Data {
    DATA.as_ref().unwrap()
}

fn run(anchors: Vec<Anchor>) -> Vec<Insight> {
    extract(data(), &input(anchors), &mut Log::off())
}

fn steps(anchors: Vec<Anchor>) -> Vec<Explanation> {
    let mut log = Log::on();
    extract(data(), &input(anchors), &mut log);
    log.into_steps()
}

fn rules(anchors: Vec<Anchor>) -> Vec<&'static str> {
    steps(anchors).iter().map(|s| s.rule).collect()
}

fn score_of(anchors: Vec<Anchor>) -> Option<i32> {
    steps(anchors)
        .iter()
        .find(|s| s.rule == "score")
        .and_then(|s| s.score)
}

fn action(item: &Insight) -> LinkAction {
    match &item.kind {
        InsightKind::Action { action, .. } => *action,
        other => panic!("not an action: {other:?}"),
    }
}

fn url(item: &Insight) -> &str {
    match &item.kind {
        InsightKind::Action { url, .. } => url.as_str(),
        other => panic!("not an action: {other:?}"),
    }
}

const PLAIN: &str = "https://example.com/account";

#[test]
fn each_weight_adds_up() {
    assert_eq!(score_of(vec![anchor(0, PLAIN, "Verify")]), Some(400 + 50));
    assert_eq!(
        score_of(vec![button(0, PLAIN, "Verify")]),
        Some(400 + 150 + 50)
    );
    assert_eq!(
        score_of(vec![anchor(0, "https://example.com/verify", "Verify")]),
        Some(400 + 150 + 50)
    );
    let aria = Anchor {
        aria_label: Some("Verify your email".into()),
        ..anchor(0, PLAIN, "")
    };
    assert_eq!(score_of(vec![aria]), Some(300 + 50));
    assert_eq!(
        score_of(vec![anchor(0, "https://example.com/verify", "")]),
        Some(150 + 50)
    );
}

#[test]
fn first_bonus_goes_to_the_first_anchor_with_an_action() {
    let all = steps(vec![
        anchor(0, PLAIN, "Read more"),
        button(1, PLAIN, "Confirm"),
        button(2, "https://example.com/b", "Confirm"),
    ]);
    let first: Vec<Option<usize>> = all
        .iter()
        .filter(|s| s.rule == "first")
        .map(|s| s.at)
        .collect();
    assert_eq!(first, [Some(1)]);
}

#[test]
fn likely_needs_six_hundred() {
    let items = run(vec![button(0, PLAIN, "Verify your email")]);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].score, 600);
    assert_eq!(items[0].confidence, Confidence::Likely);
    assert_eq!(items[0].provenance, Provenance::Heuristic);
    assert_eq!(items[0].origin, PartRef::Html);
    assert!(!items[0].forwarded);
    assert!(run(vec![anchor(0, PLAIN, "Verify your email")]).is_empty());
}

#[test]
fn scores_clamp_to_one_thousand() {
    let full = Anchor {
        aria_label: Some("Reset password".into()),
        ..button(0, "https://example.com/reset", "Reset password")
    };
    let items = run(vec![full]);
    assert_eq!(items[0].score, 1000);
    assert_eq!(action(&items[0]), LinkAction::ResetPassword);
}

#[test]
fn footer_costs_four_hundred() {
    let footer = Anchor {
        in_footer: true,
        ..button(0, "https://example.com/verify", "Verify")
    };
    assert_eq!(score_of(vec![footer.clone()]), Some(750 - 400));
    assert!(rules(vec![footer.clone()]).contains(&"footer"));
    assert!(run(vec![footer]).is_empty());
}

#[test]
fn social_hosts_cost_three_hundred() {
    let social = button(0, "https://www.facebook.com/example/verify", "Verify");
    assert_eq!(score_of(vec![social.clone()]), Some(750 - 300));
    assert!(run(vec![social]).is_empty());
    let wrapped = button(
        0,
        "https://www.google.com/url?q=https%3A%2F%2Ft.me%2Fexample%2Fverify",
        "Verify",
    );
    assert!(rules(vec![wrapped.clone()]).contains(&"social"));
    assert!(run(vec![wrapped]).is_empty());
    assert!(
        run(vec![button(
            0,
            "https://notfacebook.example/verify",
            "Verify"
        )])
        .len()
            == 1
    );
}

#[test]
fn mailto_scores_zero() {
    let mail = Anchor {
        aria_label: Some("Confirm".into()),
        ..button(0, "mailto:confirm@example.com?subject=confirm", "Confirm")
    };
    assert!(run(vec![mail.clone()]).is_empty());
    let all = steps(vec![mail]);
    assert_eq!(all[0].rule, "mailto");
    assert_eq!(all[0].score, Some(0));
}

#[test]
fn unsubscribe_preference_and_privacy_score_zero() {
    for href in [
        "https://example.com/unsubscribe?token=1",
        "https://example.com/email/preferences",
        "https://example.com/privacy",
        "https://eur01.safelinks.protection.outlook.com/?url=https%3A%2F%2Fexample.com%2Funsubscribe&data=1",
    ] {
        let a = Anchor {
            aria_label: Some("Confirm".into()),
            ..button(0, href, "Confirm")
        };
        assert!(run(vec![a.clone()]).is_empty(), "{href}");
        assert_eq!(rules(vec![a])[0], "unsubscribe", "{href}");
    }
}

#[test]
fn wallet_links_are_wallet_save_whatever_the_text() {
    let wallet =
        "https://pay.google.com/gp/v/save/eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOiJleGFtcGxlIn0.c2ln";
    let items = run(vec![anchor(0, wallet, "Sign in")]);
    assert_eq!(items.len(), 1);
    assert_eq!(action(&items[0]), LinkAction::WalletSave);
    assert_eq!(items[0].score, 600);
    let image_only = run(vec![anchor(0, wallet, "")]);
    assert_eq!(action(&image_only[0]), LinkAction::WalletSave);
    assert!(rules(vec![anchor(0, wallet, "")]).contains(&"wallet"));
    let footer = Anchor {
        in_footer: true,
        ..anchor(0, wallet, "")
    };
    assert!(run(vec![footer]).is_empty());
    let other = run(vec![button(0, PLAIN, "Add to Google Wallet")]);
    assert_eq!(action(&other[0]), LinkAction::WalletSave);
}

#[test]
fn forwarded_anchors_are_skipped() {
    let below = Anchor {
        below_forward: true,
        aria_label: Some("Reset password".into()),
        ..button(0, "https://example.com/reset", "Reset password")
    };
    assert!(run(vec![below.clone()]).is_empty());
    assert_eq!(rules(vec![below.clone()]), ["forwarded", "candidates"]);
    let above = button(1, "https://example.org/verify", "Verify");
    let items = run(vec![below, above]);
    assert_eq!(items.len(), 1);
    assert_eq!(url(&items[0]), "https://example.org/verify");
}

#[test]
fn top_two_with_distinct_urls() {
    let items = run(vec![
        button(0, "https://example.com/verify", "Verify"),
        button(1, "https://example.com/verify", "Verify"),
        button(2, "https://example.com/login", "Sign in"),
        button(3, "https://example.com/track", "Track"),
    ]);
    assert_eq!(items.len(), 2);
    assert_eq!(url(&items[0]), "https://example.com/verify");
    assert_eq!(url(&items[1]), "https://example.com/login");
    assert!(items[0].score > items[1].score);
}

#[test]
fn ties_go_to_the_lower_anchor_index() {
    let items = run(vec![
        anchor(0, "https://example.com/z/verify", "Verify"),
        button(7, "https://example.com/a/verify", "Verify"),
        button(3, "https://example.com/b/verify", "Verify"),
        button(5, "https://example.com/c/verify", "Verify"),
    ]);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].score, items[1].score);
    assert_eq!(url(&items[0]), "https://example.com/b/verify");
    assert_eq!(url(&items[1]), "https://example.com/c/verify");
}

#[test]
fn label_wins_then_aria_then_path() {
    let both = Anchor {
        aria_label: Some("Track".into()),
        ..button(0, "https://example.com/reset", "Confirm")
    };
    assert_eq!(action(&run(vec![both])[0]), LinkAction::Confirm);
    let aria = Anchor {
        aria_label: Some("Track your parcel".into()),
        ..button(0, "https://example.com/reset", "Go")
    };
    assert_eq!(action(&run(vec![aria])[0]), LinkAction::Track);
    let path = Anchor {
        aria_label: Some("Open".into()),
        ..button(0, "https://example.com/reset?token=1", "Go")
    };
    assert!(run(vec![path.clone()]).is_empty());
    assert_eq!(
        steps(vec![path])
            .iter()
            .find(|s| s.rule == "path_token")
            .map(|s| s.keywords.clone()),
        Some(vec!["/reset".to_owned()])
    );
}

#[test]
fn no_action_no_item() {
    let items = run(vec![
        button(0, "https://example.com/shop?token=1", "Shop now"),
        button(1, "https://example.com/sale", "See the sale"),
    ]);
    assert!(items.is_empty());
}

#[test]
fn urls_that_do_not_fit_are_dropped() {
    let long = format!("https://example.com/verify?t={}", "a".repeat(3_000));
    assert!(run(vec![button(0, &long, "Verify")]).is_empty());
    assert!(run(vec![button(0, "javascript:alert(1)", "Verify")]).is_empty());
    assert!(run(vec![button(0, "/verify", "Verify")]).is_empty());
}

#[test]
fn fields_of_an_item() {
    let items = run(vec![button(
        0,
        "https://example.com/verify",
        "  Verify email  ",
    )]);
    assert_eq!(
        items[0].kind,
        InsightKind::Action {
            action: LinkAction::Verify,
            url: Url::parse("https://example.com/verify").unwrap(),
            destination: Url::parse("https://example.com/verify").unwrap(),
            via: None,
            tracked: false,
            text_host: None,
            label: Some("Verify email".into()),
        }
    );
    let empty = Anchor {
        aria_label: Some("Verify".into()),
        ..button(0, "https://example.com/verify", " ")
    };
    match &run(vec![empty])[0].kind {
        InsightKind::Action { label, .. } => assert_eq!(*label, None),
        _ => unreachable!(),
    }
    let long = format!("Verify {}", "x".repeat(600));
    match &run(vec![button(0, "https://example.com/verify", &long)])[0].kind {
        InsightKind::Action { label, .. } => {
            assert_eq!(label.as_ref().unwrap().chars().count(), MAX_DISPLAY_CHARS)
        }
        _ => unreachable!(),
    }
}

#[test]
fn text_host_reads_hosts_and_urls() {
    assert_eq!(text_host("example.com"), Some("example.com".into()));
    assert_eq!(
        text_host(" www.example.org/ "),
        Some("www.example.org".into())
    );
    assert_eq!(
        text_host("https://Accounts.Example.com/"),
        Some("accounts.example.com".into())
    );
    assert_eq!(text_host("http://example.org"), Some("example.org".into()));
    assert_eq!(
        text_host("bücher.example"),
        Some("xn--bcher-kva.example".into())
    );
    assert_eq!(text_host("exаmple.com"), Some("xn--exmple-4nf.com".into()));
    for (url, host) in [
        ("example.com/reset", "example.com"),
        ("https://www.Bank.example/login", "www.bank.example"),
        (
            "https://accounts.example.com/login?next=/home#top",
            "accounts.example.com",
        ),
        ("example.com:8080", "example.com"),
        ("http://example.org:8443/a/b", "example.org"),
        ("example.org?ref=mail", "example.org"),
    ] {
        assert_eq!(text_host(url), Some(host.into()), "{url}");
    }
    for not in [
        "",
        "Verify",
        "Verify example.com",
        "ada@example.com",
        "https://ada@example.com/reset",
        "https://example.com/reset now",
        "1.5/10",
        "192.168.1.1/admin",
        "3.50",
        "192.168.1.1",
        "example",
        "example.com:port",
        "https://",
    ] {
        assert_eq!(text_host(not), None, "{not}");
    }
}

#[test]
fn a_label_per_language() {
    for (text, expected) in [
        ("Verify email", LinkAction::Verify),
        ("Подтвердить почту", LinkAction::Verify),
        ("Passwort zurücksetzen", LinkAction::ResetPassword),
        ("Réinitialiser le mot de passe", LinkAction::ResetPassword),
        ("Iniciar sesión", LinkAction::SignIn),
        ("Rastrear pedido", LinkAction::Track),
        ("Attiva account", LinkAction::Activate),
        ("查看订单", LinkAction::ViewOrder),
        ("チェックイン", LinkAction::CheckIn),
        ("비밀번호 재설정", LinkAction::ResetPassword),
        ("Siparişi görüntüle", LinkAction::ViewOrder),
        ("Potwierdź", LinkAction::Confirm),
        ("ＬＯＧ ＩＮ", LinkAction::SignIn),
        ("Get your magic link", LinkAction::MagicLink),
    ] {
        let items = run(vec![button(0, PLAIN, text)]);
        assert_eq!(items.len(), 1, "{text}");
        assert_eq!(action(&items[0]), expected, "{text}");
    }
}

#[test]
fn only_html_bodies() {
    let text_only = InsightInput {
        text: "Verify your email: https://example.com/verify".into(),
        ..InsightInput::default()
    };
    assert!(extract(data(), &text_only, &mut Log::off()).is_empty());
}

#[test]
fn only_the_first_anchors_are_read() {
    let mut anchors: Vec<Anchor> = (0..MAX_ANCHORS as u32)
        .map(|i| anchor(i, PLAIN, "Read more"))
        .collect();
    anchors.push(button(9_999, "https://example.com/verify", "Verify"));
    assert!(run(anchors).is_empty());
}
