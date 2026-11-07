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
        for link in label.links.iter().flatten() {
            assert!(
                !report.contains(link.host.as_str()),
                "{case}: a link host in the report"
            );
        }
        for parcel in label.parcel.iter().flatten() {
            assert!(
                !report.contains(parcel.number.as_str()),
                "{case}: a tracking number in the report"
            );
        }
        for fact in label.structured.iter().flatten() {
            assert!(
                !report.contains(fact.reference.as_str()),
                "{case}: a {} reference in the report",
                fact.kind
            );
        }
        for wrapper in ["outlook.com", "proofpoint.com", "google.com", "sendgrid"] {
            assert!(!report.contains(wrapper), "{case}: {wrapper} in the report");
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

    let report = runner.explain(&common::parse("link-reset-safelinks.eml"));
    assert!(report.contains("[links "), "{report}");
    assert!(
        report.contains("action=reset_password via=true tracked=false"),
        "{report}"
    );

    let report = runner.explain(&common::parse("parcel-ups-en.eml"));
    assert!(report.contains("[parcel "), "{report}");
    assert!(report.contains("carrier=ups shape="), "{report}");

    let report = runner.explain(&common::parse("sx-flight-graph.eml"));
    assert!(report.contains("[schemaorg "), "{report}");
    assert!(
        report.contains(
            "reservation certain json-ld score=1000 origin=html reservation=flight number=aDaaDa start=zoned people=1 seats=1 url=true"
        ),
        "{report}"
    );

    let report = runner.explain(&common::parse("sx-pkpass-boarding.eml"));
    assert!(report.contains("[pass "), "{report}");
    assert!(
        report.contains("pass certain pkpass score=1000 origin=attachment:0 style=boarding_pass attachment=0 relevant=zoned boarding_legs=1"),
        "{report}"
    );

    let report = runner.explain(&common::parse("sx-neg-booking-code.eml"));
    assert!(report.contains("outcome:likely"), "{report}");
    assert!(!report.contains("code likely"), "{report}");
}
