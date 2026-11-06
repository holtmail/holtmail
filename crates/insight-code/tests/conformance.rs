//! The conformance suite and the §15 fixtures, built as `InsightInput` values.

use holtmail_core::{Address, Timestamp};
use holtmail_insight::testing::{Case, Expect, conformance};
use holtmail_insight::{
    Anchor, Confidence, HeaderField, HtmlView, InsightExtractor, InsightInput, InsightKind,
    PartRef, Provenance, Segment,
};
use holtmail_insight_code::CodeExtractor;

struct Sample {
    name: &'static str,
    input: InsightInput,
    code: Option<&'static str>,
}

fn headers(subject: &str, extra: &[(&str, &str)]) -> Vec<HeaderField> {
    let mut out = vec![
        HeaderField::new("From", "Example Service <noreply@example.com>"),
        HeaderField::new("To", "Ada Example <ada@example.org>"),
        HeaderField::new("Subject", subject),
        HeaderField::new("Message-ID", "<otp-1@example.com>"),
        HeaderField::new("Date", "Fri, 06 Nov 2026 09:12:00 +0000"),
    ];
    out.extend(extra.iter().map(|(n, v)| HeaderField::new(*n, *v)));
    out
}

fn mail(subject: &str, text: &str) -> InsightInput {
    InsightInput {
        headers: headers(subject, &[]),
        from: vec![Address {
            name: Some("Example Service".into()),
            email: "noreply@example.com".into(),
        }],
        subject: Some(subject.into()),
        sent_at: Some(Timestamp::from_secs(1_793_952_720)),
        text: text.into(),
        ..InsightInput::default()
    }
}

fn with_headers(mut input: InsightInput, extra: &[(&str, &str)]) -> InsightInput {
    let subject = input.subject.clone().unwrap_or_default();
    input.headers = headers(&subject, extra);
    input
}

fn html(input: InsightInput, segments: Vec<Segment>, footer_at: Option<usize>) -> InsightInput {
    InsightInput {
        html: Some(
            HtmlView::new()
                .with_segments(segments)
                .with_footer_at(footer_at),
        ),
        ..input
    }
}

fn seg(text: &str) -> Segment {
    Segment {
        text: text.into(),
        alone_on_line: true,
        ..Segment::default()
    }
}

fn inline(text: &str) -> Segment {
    Segment {
        text: text.into(),
        ..Segment::default()
    }
}

fn strong(text: &str) -> Segment {
    Segment {
        text: text.into(),
        emphasis: true,
        alone_on_line: true,
        below_forward: false,
    }
}

fn sample(name: &'static str, input: InsightInput, code: Option<&'static str>) -> Sample {
    Sample { name, input, code }
}

fn fixtures() -> Vec<Sample> {
    let forwarded_text = "FYI, see below.\n\n---------- Forwarded message ---------\nFrom: Example \
                          Service <noreply@example.com>\nSubject: Your code\n\nYour verification \
                          code is 482913.\n";
    let forward_at = forwarded_text
        .find("----------")
        .map(|at| forwarded_text[..at].chars().count());

    vec![
        sample(
            "otp-plain-en",
            mail(
                "Your verification code",
                "Hello,\n\nYour verification code is:\n\n482913\n\nIt expires in 10 minutes. If \
                 you did not ask for it, ignore this message.\n\nExample Service\n",
            ),
            Some("482913"),
        ),
        sample(
            "otp-ru-html-strong",
            html(
                mail(
                    "Код подтверждения",
                    "Здравствуйте!\n\nВаш код подтверждения: 4821\n\nКод действует 10 минут.\n",
                ),
                vec![
                    seg("Здравствуйте!"),
                    seg("Ваш код подтверждения:"),
                    strong("4821"),
                    seg("Код действует 10 минут."),
                ],
                None,
            ),
            Some("4821"),
        ),
        sample(
            "otp-split-dash",
            mail("Sign in to Example", "Your sign-in code: 123-456\n"),
            Some("123456"),
        ),
        sample(
            "otp-near-order-number",
            mail(
                "Confirm it is you",
                "Your verification code is 482913.\nYour order 12345678 has shipped.\n",
            ),
            Some("482913"),
        ),
        sample(
            "otp-price-date-phone",
            mail(
                "Security code",
                "Your security code is 739104.\nTotal charged: $1,299.00 on 2026-11-06.\n\
                 Questions? Call +1 555 0100 2233.\n",
            ),
            Some("739104"),
        ),
        sample(
            "otp-in-href-only",
            InsightInput {
                html: Some(
                    HtmlView::new()
                        .with_segments(vec![
                            seg("Confirm your email"),
                            seg("Press the button below to verify your address."),
                            seg("Verify"),
                        ])
                        .with_anchors(vec![Anchor {
                            href: "https://example.com/verify?code=482913".into(),
                            text: "Verify".into(),
                            button_like: true,
                            ..Anchor::default()
                        }]),
                ),
                ..mail(
                    "Confirm your email",
                    "Confirm your email\n\nOpen https://example.com/verify?code=482913 to verify \
                     your address.\n",
                )
            },
            None,
        ),
        sample(
            "otp-in-href-only-text",
            mail(
                "Confirm your email",
                "Open https://example.com/verify?code=482913 to verify your address.\n",
            ),
            None,
        ),
        sample(
            "otp-in-footer",
            html(
                mail("Your login code", ""),
                vec![
                    seg("Your login code"),
                    strong("583920"),
                    seg("Valid for 15 minutes."),
                    inline("Unsubscribe"),
                    seg("Example Inc., 1 Example Street, Springfield, support PIN 49007"),
                ],
                Some(3),
            ),
            Some("583920"),
        ),
        sample(
            "otp-only-in-footer",
            html(
                mail("Security settings", ""),
                vec![
                    seg("Your security code settings were updated."),
                    inline("©"),
                    seg("Example Inc. Support PIN 7744"),
                ],
                Some(1),
            ),
            None,
        ),
        sample(
            "otp-hex-color",
            mail(
                "Access code",
                "Your access code is 604918.\nTheme colour #a1b2c3 and #00ff99.\n",
            ),
            Some("604918"),
        ),
        sample(
            "otp-hex-color-alphanumeric",
            mail(
                "Access code",
                "Your access code: K7P2QX\nTheme colour #a1b2c3.\n",
            ),
            Some("K7P2QX"),
        ),
        sample(
            "otp-newsletter-year",
            mail(
                "Our November 2026 update",
                "Our code of conduct was updated in November 2026. Read what changed.\n",
            ),
            None,
        ),
        sample(
            "otp-tie-two-codes",
            mail("Your code", "Your code is 482913 or 739104.\n"),
            None,
        ),
        sample(
            "otp-zh-no-space",
            mail(
                "验证码通知",
                "您的验证码是123456，5分钟内有效。请勿泄露。\n",
            ),
            Some("123456"),
        ),
        sample(
            "otp-ja-fullwidth",
            mail(
                "認証コードのお知らせ",
                "認証コード：８７６５４３\n有効期限は10分です。\n",
            ),
            Some("876543"),
        ),
        sample(
            "otp-ko",
            mail("인증번호 안내", "인증번호 [384920]를 입력해 주세요.\n"),
            Some("384920"),
        ),
        sample(
            "otp-subject-and-body-same",
            mail(
                "482913 is your verification code",
                "Your verification code is 482913.\n",
            ),
            Some("482913"),
        ),
        sample(
            "otp-backup-codes",
            mail(
                "Your backup codes",
                "Keep these backup codes safe:\n\n48291736\n90172245\n33861902\n71520048\n",
            ),
            None,
        ),
        sample(
            "otp-code-list",
            mail(
                "Your sign-in codes",
                "Your sign-in codes:\n48291736\n90172245\n33861902\n",
            ),
            None,
        ),
        sample(
            "otp-promo-code-newsletter",
            mail(
                "Weekend sale",
                "Save 20% this weekend.\nUse promo code SAVE2026 at checkout.\n",
            ),
            None,
        ),
        sample(
            "otp-promo-code-ru",
            mail("Скидка недели", "Ваш промокод: 7734\n"),
            None,
        ),
        sample(
            "otp-barcode-word",
            mail(
                "Your pickup code",
                "Scan the Code128 barcode at the counter.\nYour pickup code is 7391.\n\
                 Barcode 40063813\n",
            ),
            Some("7391"),
        ),
        sample(
            "otp-text-html-disagree",
            html(
                mail("Your code", "Your code is 111222\n"),
                vec![seg("Your code is"), strong("583920")],
                None,
            ),
            Some("583920"),
        ),
        sample(
            "otp-css-in-style-block",
            html(
                mail(
                    "Sign in to Example",
                    ".code{letter-spacing:4px;color:#1a2b3c}\nYour sign-in code is 583920\n",
                ),
                vec![
                    seg("Sign in to Example"),
                    seg("Your sign-in code is"),
                    strong("583920"),
                ],
                None,
            ),
            Some("583920"),
        ),
        sample(
            "otp-css-in-text",
            mail(
                "Sign in to Example",
                ".code{letter-spacing:4px;color:#1a2b3c}\nYour sign-in code is 583920\n",
            ),
            Some("583920"),
        ),
        sample(
            "fwd-inline-otp",
            InsightInput {
                forward_at,
                ..mail("Fwd: Your code", forwarded_text)
            },
            None,
        ),
    ]
}

fn languages() -> Vec<Sample> {
    vec![
        sample(
            "lang-ru-plain",
            mail("Вход в аккаунт", "Ваш код: 5821\n"),
            Some("5821"),
        ),
        sample(
            "lang-de",
            mail(
                "Ihr Bestätigungscode",
                "Ihr Bestätigungscode lautet: 529174. Er ist 10 Minuten gültig.\n",
            ),
            Some("529174"),
        ),
        sample(
            "lang-fr",
            mail(
                "Code de vérification",
                "Votre code de vérification est : 318402\n",
            ),
            Some("318402"),
        ),
        sample(
            "lang-es",
            mail("Tu código", "Tu código de verificación es 760215\n"),
            Some("760215"),
        ),
        sample(
            "lang-pt",
            mail(
                "Seu código",
                "Seu código de verificação é 904417. Não compartilhe.\n",
            ),
            Some("904417"),
        ),
        sample(
            "lang-it",
            mail("Codice di verifica", "Il tuo codice di verifica è 271863\n"),
            Some("271863"),
        ),
        sample(
            "lang-tr",
            mail("Doğrulama", "Doğrulama kodunuz: 650921\n"),
            Some("650921"),
        ),
        sample(
            "lang-pl",
            mail("Kod weryfikacyjny", "Twój kod weryfikacyjny to 447190\n"),
            Some("447190"),
        ),
    ]
}

fn negatives() -> Vec<Sample> {
    vec![
        sample(
            "neg-year-near-month",
            mail(
                "Code of conduct",
                "Our code of conduct changes on 6 November 2026.\n",
            ),
            None,
        ),
        sample(
            "neg-money",
            mail(
                "Gift code applied",
                "Your code was applied. Total: $ 1250\n",
            ),
            None,
        ),
        sample(
            "neg-phone",
            mail("Lost your code?", "Lost your code? Call +1 5550 1000.\n"),
            None,
        ),
        sample(
            "neg-postcode",
            mail(
                "Your access code",
                "Your access code will be posted to 12 Example Street 94105.\n",
            ),
            None,
        ),
        sample(
            "neg-reference-prefix",
            mail(
                "Order update",
                "Order #48291736 shipped. Use the code from your app to track it.\n",
            ),
            None,
        ),
        sample(
            "neg-list-id",
            with_headers(
                mail("Your code", "Your code is 482913\n"),
                &[("List-Id", "<news.example.com>")],
            ),
            None,
        ),
        sample(
            "list-id-auto-submitted",
            with_headers(
                mail("Your code", "Your code is 482913\n"),
                &[
                    ("List-Id", "<news.example.com>"),
                    ("Auto-Submitted", "auto-generated"),
                ],
            ),
            Some("482913"),
        ),
        sample(
            "neg-bulk-alphanumeric",
            with_headers(
                mail("Your code", "Your code is K7P2QX\n"),
                &[("Precedence", "bulk")],
            ),
            None,
        ),
        sample(
            "neg-no-keyword",
            mail("Meeting", "Room 4512 at noon.\n"),
            None,
        ),
    ]
}

fn all() -> Vec<Sample> {
    let mut out = fixtures();
    out.extend(languages());
    out.extend(negatives());
    out
}

#[test]
fn keeps_the_contract() {
    let cases: Vec<Case> = all()
        .into_iter()
        .map(|s| {
            let expect = if s.code.is_some() {
                Expect::Found
            } else {
                Expect::Nothing
            };
            Case::new(s.name, s.input, expect)
        })
        .chain([Case::new(
            "otp-wicg-origin-bound",
            origin_bound(),
            Expect::Found,
        )])
        .collect();
    conformance::extractor(&CodeExtractor::new(), &cases);
}

#[test]
fn finds_the_expected_code() {
    let extractor = CodeExtractor::new();
    for s in all() {
        let items = extractor.extract(&s.input);
        let values: Vec<&str> = items
            .iter()
            .filter_map(|item| match &item.kind {
                InsightKind::Code { value, .. } => Some(value.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(values, s.code.into_iter().collect::<Vec<_>>(), "{}", s.name);
        for item in &items {
            assert!(!item.forwarded, "{}", s.name);
            assert_eq!(item.provenance, Provenance::Heuristic, "{}", s.name);
            let likely = item.score >= 600;
            assert_eq!(
                item.confidence,
                if likely {
                    Confidence::Likely
                } else {
                    Confidence::Possible
                },
                "{}",
                s.name
            );
        }
    }
}

fn origin_bound() -> InsightInput {
    mail(
        "Your code",
        "Your Example code is 482913.\n\n@example.com #482913\n",
    )
}

#[test]
fn origin_bound_is_certain() {
    let items = CodeExtractor::new().extract(&origin_bound());
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(
        item.kind,
        InsightKind::Code {
            value: "482913".into(),
            origin_bound: Some("example.com".into()),
        }
    );
    assert_eq!(item.confidence, Confidence::Certain);
    assert_eq!(item.provenance, Provenance::OriginBound);
    assert_eq!(item.score, 1000);
}

#[test]
fn plain_code_on_its_own_line_is_likely() {
    let items = CodeExtractor::new().extract(&fixtures().remove(0).input);
    assert_eq!(items[0].confidence, Confidence::Likely);
    assert_eq!(items[0].origin, PartRef::Text);
}

#[test]
fn html_wins_and_subject_merges() {
    let extractor = CodeExtractor::new();
    let by_name = |name: &str| {
        all()
            .into_iter()
            .find(|s| s.name == name)
            .map(|s| extractor.extract(&s.input))
            .unwrap()
    };
    assert_eq!(by_name("otp-text-html-disagree")[0].origin, PartRef::Html);
    let same = by_name("otp-subject-and-body-same");
    assert_eq!(same.len(), 1);
    assert_eq!(same[0].origin, PartRef::Text);
    assert_eq!(same[0].confidence, Confidence::Likely);
}

#[test]
fn version_carries_the_table_hash() {
    let version = CodeExtractor::new().version();
    let (algorithm, hash) = version.split_once('+').unwrap();
    assert_eq!(algorithm, "2");
    assert_eq!(hash.len(), 12);
    assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(CodeExtractor::new().id(), "code");
}
