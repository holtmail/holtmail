//! Raw-text, hidden-content, forward and footer fixtures.

use holtmail_html_scan::scan;
use holtmail_insight::limits::{MAX_MICRODATA_DEPTH, MAX_MICRODATA_PROPERTIES};
use holtmail_insight::{HtmlView, MicrodataValue};

fn fixture(name: &str) -> HtmlView {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    scan(&std::fs::read_to_string(path).expect("fixture"))
}

fn text(view: &HtmlView) -> String {
    view.segments
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" | ")
}

fn hrefs(view: &HtmlView) -> Vec<&str> {
    view.anchors.iter().map(|a| a.href.as_str()).collect()
}

fn assert_raw_element(name: &str) {
    let view = fixture(name);
    let all = text(&view);
    assert!(!all.contains("trap"), "{name}: {all}");
    assert!(!all.contains("482913"), "{name}: {all}");
    assert!(!all.contains("Hidden"), "{name}: {all}");
    assert_eq!(hrefs(&view), ["https://example.com/after"], "{name}");
    assert!(all.contains("After link"), "{name}: {all}");
}

#[test]
fn rawtext_elements() {
    for element in ["style", "xmp", "iframe", "noembed", "noframes", "noscript"] {
        assert_raw_element(&format!("rawtext-{element}.html"));
    }
}

#[test]
fn rcdata_elements() {
    for element in ["title", "textarea"] {
        assert_raw_element(&format!("rcdata-{element}.html"));
    }
}

#[test]
fn script_data() {
    assert_raw_element("script.html");
    assert_raw_element("script-escaped.html");
}

#[test]
fn script_ld_json_feeds_ld_json() {
    let view = fixture("script-ld-json.html");
    assert_eq!(view.ld_json.len(), 1);
    assert!(view.ld_json[0].contains("\"reservationNumber\":\"RXJ34P\""));
    assert!(view.ld_json[0].contains("<a href='https://example.com/hidden'>"));
    assert_eq!(hrefs(&view), ["https://example.com/after"]);
    assert!(!text(&view).contains("RXJ34P"));
}

#[test]
fn plaintext_takes_the_rest() {
    let view = fixture("plaintext.html");
    assert_eq!(text(&view), "Before plaintext");
    assert!(view.anchors.is_empty());
}

#[test]
fn hidden_display_none_anchor() {
    let view = fixture("hidden-display-none-anchor.html");
    assert_eq!(hrefs(&view), ["https://example.com/confirm?t=abc"]);
    let anchor = &view.anchors[0];
    assert_eq!(anchor.text, "Confirm your address");
    assert!(anchor.button_like);
    assert_eq!(anchor.index, 0);
    let all = text(&view);
    assert!(!all.contains("Preheader"), "{all}");
    assert_eq!(all, "Confirm your address | Thanks, Example Team");
}

#[test]
fn hidden_template_code() {
    let view = fixture("hidden-template-code.html");
    let all = text(&view);
    assert!(!all.contains("482913"), "{all}");
    assert!(view.anchors.is_empty());
    let code = view
        .segments
        .iter()
        .find(|s| s.text == "731025")
        .expect("code segment");
    assert!(code.emphasis);
    assert!(code.alone_on_line);
    assert_eq!(
        all,
        "Your sign-in code is | 731025 | It expires in 10 minutes."
    );
}

#[test]
fn aria_hidden_and_other_hiding() {
    let view = fixture("aria-hidden.html");
    let all = text(&view);
    for hidden in ["999999", "888888", "777777", "666666", "555555"] {
        assert!(!all.contains(hidden), "{hidden}: {all}");
    }
    assert!(all.contains("Shown text"));
    assert_eq!(hrefs(&view), ["https://example.com/visible"]);
    assert_eq!(
        view.anchors[0].aria_label.as_deref(),
        Some("Open the visible page")
    );
}

#[test]
fn forward_marks_below() {
    let view = fixture("forward.html");
    let first_below = view
        .segments
        .iter()
        .position(|s| s.below_forward)
        .expect("forward");
    assert_eq!(
        view.segments[first_below].text,
        "---------- Forwarded message ---------"
    );
    assert!(
        view.segments[..first_below]
            .iter()
            .all(|s| s.text.starts_with("See below"))
    );
    let code = view
        .segments
        .iter()
        .find(|s| s.text == "482913")
        .expect("code");
    assert!(code.below_forward && code.emphasis);
    assert!(view.anchors.iter().all(|a| a.below_forward));
}

#[test]
fn footer_from_unsubscribe_anchor() {
    let view = fixture("footer.html");
    let footer = view.footer_at.expect("footer");
    assert!(view.segments[footer].text.starts_with("Unsubscribe"));
    let flags: Vec<_> = view
        .anchors
        .iter()
        .map(|a| (a.href.as_str(), a.in_footer))
        .collect();
    assert_eq!(
        flags,
        [
            ("https://example.com/read", false),
            ("https://example.com/u/abc", true),
            ("https://example.com/prefs", true),
        ]
    );
}

#[test]
fn foreign_content_is_skipped() {
    let view = fixture("foreign-content.html");
    let all = text(&view);
    for hidden in ["svg style", "111111", "222222"] {
        assert!(!all.contains(hidden), "{hidden}: {all}");
    }
    assert!(all.contains("Breakout paragraph"), "{all}");
    assert_eq!(hrefs(&view), ["https://example.com/end"]);
}

#[test]
fn elements_the_sanitizer_drops_are_skipped() {
    let view = fixture("dropped-elements.html");
    assert_eq!(text(&view), "Start | Kept outlook custom | End");
    assert!(view.anchors.is_empty());
}

#[test]
fn emphasis_and_lines() {
    let view = fixture("emphasis-and-lines.html");
    let flags: Vec<_> = view
        .segments
        .iter()
        .map(|s| (s.text.as_str(), s.emphasis, s.alone_on_line))
        .collect();
    assert_eq!(
        flags,
        [
            ("Your code:", false, true),
            ("482913", true, true),
            ("Use", false, false),
            ("731025", true, false),
            ("to sign in.", false, false),
            ("Welcome", true, true),
            ("Small heading", false, true),
            ("Bold span", true, true),
            ("Big font", true, true),
        ]
    );
}

#[test]
fn malformed_markup() {
    let view = fixture("malformed.html");
    let all = text(&view);
    for hidden in ["Hidden", "bold", "Two hidden"] {
        assert!(!all.contains(hidden), "{hidden}: {all}");
    }
    for shown in [
        "Visible after div",
        "Cell one",
        "Cell two",
        "Third",
        "One",
        "Three",
        "Tail & done",
    ] {
        assert!(all.contains(shown), "{shown}: {all}");
    }
    let anchors: Vec<_> = view
        .anchors
        .iter()
        .map(|a| (a.href.as_str(), a.text.as_str(), a.index))
        .collect();
    assert_eq!(
        anchors,
        [
            ("https://example.com/1", "First", 0),
            ("https://example.com/2", "Second", 1),
        ]
    );
}

fn items(view: &HtmlView) -> Vec<String> {
    view.microdata
        .iter()
        .map(|item| {
            let props = item
                .properties
                .iter()
                .map(|p| match &p.value {
                    MicrodataValue::Text(text) => format!("{}={text:?}", p.name),
                    MicrodataValue::Url(url) => format!("{}=<{url}>", p.name),
                    MicrodataValue::Item(i) => format!("{}=#{i}", p.name),
                })
                .collect::<Vec<_>>()
                .join(" ");
            let types = item
                .types
                .iter()
                .map(|t| t.rsplit('/').next().unwrap_or_default())
                .collect::<Vec<_>>()
                .join(",");
            let flags = format!(
                "{}{}",
                if item.top_level { "top" } else { "nested" },
                if item.below_forward { " fwd" } else { "" }
            );
            format!("{types} [{flags}] {props}")
        })
        .collect()
}

#[test]
fn microdata_lodging() {
    let view = fixture("microdata-lodging.html");
    assert_eq!(
        items(&view),
        [
            "LodgingReservation [top] reservationNumber=\"HX-4417\" \
             reservationStatus=<http://schema.org/ReservationConfirmed> reservationFor=#1 \
             checkinTime=\"2026-12-01T15:00:00+01:00\" checkoutTime=\"2 Dec 2026\" \
             price=\"240.00\" image=<https://example.org/room.jpg>",
            "LodgingBusiness [nested] name=\"Example Harbour Hotel\" address=#2 \
             url=<https://example.com/hotel>",
            "PostalAddress [nested] streetAddress=\"1 Example Quay\" addressLocality=\"Exampleton\"",
        ]
    );
    assert_eq!(view.microdata[0].id.as_deref(), Some("urn:example:res:1"));
    assert!(!view.truncated_structured);
    assert!(text(&view).contains("Example Harbour Hotel"));
}

#[test]
fn microdata_itemref() {
    let view = fixture("microdata-itemref.html");
    assert_eq!(
        items(&view),
        [
            "FoodEstablishmentReservation [top] reservationNumber=\"T-1234\" reservationFor=#1 \
             startTime=\"2026-12-24T19:30\" partySize=\"4\"",
            "FoodEstablishment [nested] name=\"Example Bistro\"",
        ]
    );
}

#[test]
fn microdata_itemref_cycle_is_cut() {
    let view = fixture("microdata-itemref-cycle.html");
    assert_eq!(
        items(&view),
        [
            "Thing [top] name=\"Alpha\" sibling=#1",
            "Thing [nested] name=\"Beta\" sibling=#2",
            "Thing [nested] sibling=#1 name=\"Gamma\"",
        ]
    );
    assert!(!view.truncated_structured);
}

#[test]
fn microdata_in_hidden_subtrees_is_kept() {
    let view = fixture("microdata-hidden-div.html");
    assert_eq!(
        items(&view),
        [
            "Order [top] orderNumber=\"EX-77812\" merchant=#1 \
             url=<https://example.com/orders/EX-77812>",
            "Organization [nested] name=\"Example Shop\"",
        ]
    );
    assert_eq!(text(&view), "Your order shipped. | Visible footer text");
    assert!(view.anchors.is_empty());
}

#[test]
fn microdata_never_comes_from_inert_or_raw_content() {
    let view = fixture("microdata-in-template.html");
    assert!(view.microdata.is_empty(), "{:?}", items(&view));
    assert!(!view.truncated_structured);
}

#[test]
fn ld_json_and_microdata_below_forward() {
    let view = fixture("ld-json-below-forward.html");
    assert_eq!(view.ld_json.len(), 3);
    assert!(view.ld_json[0].contains("ABOVE-1"));
    assert_eq!(view.ld_json_forward_at, Some(1));
    assert_eq!(
        items(&view),
        [
            "Thing [top] name=\"above\"",
            "Order [top fwd] orderNumber=\"BELOW-2\"",
        ]
    );

    let plain = fixture("script-ld-json.html");
    assert_eq!(plain.ld_json_forward_at, None);
}

#[test]
fn microdata_fan_in_bomb_stays_in_budget() {
    let big = (0..1_000)
        .map(|i| format!("<span itemprop=\"part\">p{i}</span>"))
        .collect::<String>();
    let refs = "<div itemscope itemtype=\"https://schema.org/Thing\" itemref=\"big\"></div>\n"
        .repeat(1_000);
    let view = scan(&format!("<div id=\"big\">{big}</div>{refs}"));
    let total: usize = view.microdata.iter().map(|i| i.properties.len()).sum();
    assert_eq!(total, MAX_MICRODATA_PROPERTIES);
    assert!(view.truncated_structured);
    assert_eq!(view.microdata[0].properties.len(), 1_000);
    assert_eq!(
        view.microdata[0].properties[999].value,
        MicrodataValue::Text("p999".into())
    );
}

#[test]
fn microdata_nested_depth_20_is_capped() {
    let depth = 20;
    let html = format!(
        "{}<span itemprop=\"leaf\">deep</span>{}",
        (0..depth)
            .map(|i| format!(
                "<div itemprop=\"child\" itemscope itemtype=\"https://schema.org/T{i}\">"
            ))
            .collect::<String>(),
        "</div>".repeat(depth)
    );
    let view = scan(&html);
    assert_eq!(view.microdata.len(), MAX_MICRODATA_DEPTH);
    assert!(view.truncated_structured);
    for (i, item) in view
        .microdata
        .iter()
        .enumerate()
        .take(MAX_MICRODATA_DEPTH - 1)
    {
        assert_eq!(item.properties.len(), 1);
        assert_eq!(item.properties[0].value, MicrodataValue::Item(i + 1));
    }
    let deepest = view.microdata.last().expect("deepest");
    let names: Vec<_> = deepest.properties.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["child", "child", "child", "child", "leaf"]);
    assert_eq!(
        deepest.properties[4].value,
        MicrodataValue::Text("deep".into())
    );
    assert_eq!(
        deepest.properties[0].value,
        MicrodataValue::Text("deep".into())
    );
}
