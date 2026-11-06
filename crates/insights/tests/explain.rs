//! `Runner::explain` over the corpus: rule names, positions, scores and shapes, never values,
//! hosts or addresses (docs/INSIGHTS.md §12).

mod common;

#[test]
fn reports_never_carry_values_or_hosts() {
    let runner = common::runner();
    for label in common::labels() {
        let report = runner.explain(&common::parse(&label.file));
        let case = &label.file;
        assert!(!report.contains("example."), "{case}: a host in the report");
        assert!(!report.contains('@'), "{case}: an address in the report");
        if let Some(code) = &label.code {
            assert!(
                !report.contains(code.as_str()),
                "{case}: the code in the report"
            );
        }
        if let Some(unsubscribe) = &label.unsubscribe {
            for link in [&unsubscribe.https, &unsubscribe.http, &unsubscribe.mailto]
                .into_iter()
                .flatten()
            {
                assert!(
                    !report.contains(link.as_str()),
                    "{case}: a link in the report"
                );
            }
        }
    }
}

#[test]
fn reports_name_the_rules() {
    let runner = common::runner();
    let report = runner.explain(&common::parse("otp-wicg-origin-bound.eml"));
    assert!(report.contains("[code "), "{report}");
    assert!(
        report.contains("origin_bound text score=1000 shape=DDDDDD"),
        "{report}"
    );
    assert!(
        report.contains("code certain origin-bound score=1000"),
        "{report}"
    );

    let report = runner.explain(&common::parse("otp-near-order-number.eml"));
    assert!(report.contains("reference_prefix text"), "{report}");
    assert!(report.contains("outcome:likely"), "{report}");

    let report = runner.explain(&common::parse("unsub-one-click-covered.eml"));
    assert!(
        report.contains("item:https header:List-Unsubscribe at=0"),
        "{report}"
    );
    assert!(report.contains("outcome:one_click"), "{report}");
    assert!(report.contains("one_click=true"), "{report}");
}
