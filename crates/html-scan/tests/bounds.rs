//! Caps and nesting bounds of the scan.

use holtmail_html_scan::scan;
use holtmail_insight::limits::{
    MAX_ANCHORS, MAX_HTML_BYTES, MAX_LD_JSON_BLOCKS, MAX_LD_JSON_BYTES, MAX_SEGMENTS,
};

#[test]
fn deep_nesting_stays_flat() {
    let depth = 20_000;
    let html = format!("{}deep{}", "<div>".repeat(depth), "</div>".repeat(depth));
    let view = scan(&html);
    assert_eq!(view.segments.len(), 1);
    assert_eq!(view.segments[0].text, "deep");

    let html = format!("{}<b>x</b>", "<span><i>".repeat(5_000));
    assert_eq!(scan(&html).segments.len(), 1);
}

#[test]
fn hidden_below_the_depth_cap_stays_hidden() {
    let html = format!(
        "{}<span style=\"display:none\">secret</span>{}<p>shown</p>",
        "<div>".repeat(300),
        "</div>".repeat(300)
    );
    let view = scan(&html);
    let text: Vec<_> = view.segments.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(text, ["shown"]);

    let html = format!("{}<template>inert</template>after", "<div>".repeat(300));
    assert!(scan(&html).segments.is_empty());
}

#[test]
fn anchors_are_capped() {
    let html = (0..MAX_ANCHORS + 50)
        .map(|i| format!("<a href=\"https://example.com/{i}\">link {i}</a> "))
        .collect::<String>();
    let view = scan(&html);
    assert_eq!(view.anchors.len(), MAX_ANCHORS);
    assert!(
        view.anchors
            .iter()
            .enumerate()
            .all(|(i, a)| a.index as usize == i)
    );
}

#[test]
fn segments_are_capped() {
    let html = (0..MAX_SEGMENTS + 50)
        .map(|i| format!("<p>line {i}</p>"))
        .collect::<String>();
    assert_eq!(scan(&html).segments.len(), MAX_SEGMENTS);
}

#[test]
fn ld_json_is_capped_and_never_cut() {
    let block = "<script type=\"application/ld+json\">{\"@type\":\"Order\"}</script>";
    let view = scan(&block.repeat(MAX_LD_JSON_BLOCKS + 3));
    assert_eq!(view.ld_json.len(), MAX_LD_JSON_BLOCKS);

    let big = format!(
        "<script type=\"Application/LD+JSON; charset=utf-8\">\"{}\"</script>{block}",
        "x".repeat(MAX_LD_JSON_BYTES)
    );
    let view = scan(&big);
    assert_eq!(view.ld_json, ["{\"@type\":\"Order\"}"]);

    let other = "<script type=\"text/javascript\">{}</script><template><script type=\"application/ld+json\">{}</script></template>";
    assert!(scan(other).ld_json.is_empty());
}

#[test]
fn input_is_capped_on_a_char_boundary() {
    let mut html = "é".repeat(MAX_HTML_BYTES / 2 - 1);
    html.push_str("x<p>after the cap</p>");
    let view = scan(&html);
    assert!(view.segments.iter().all(|s| !s.text.contains("after")));
}

#[test]
fn hostile_input_never_panics() {
    for html in [
        "",
        "<",
        "</",
        "<a",
        "<a href=\"https://example.com/",
        "<!--",
        "<![CDATA[",
        "<script>",
        "<script><!--<script>",
        "<plaintext>",
        "&#xD800;&#0;&#x110000;&amp",
        "\u{FFFD}\u{0}\u{FEFF}<p>\u{FFFD}</p>",
        "</p></br></table></template></svg></math></a>",
        "<svg><svg><svg><foreignobject><math><mi><svg>",
        "<table><table><table><td><td><tr><caption><colgroup><col>",
        "<a href=https://example.com/1><a href=https://example.com/2><a>",
    ] {
        let view = scan(html);
        assert!(view.anchors.len() <= MAX_ANCHORS);
    }
}

#[test]
fn output_is_deterministic() {
    let html = "<p>Code <b>482913</b></p><a href=\"https://example.com/x\">x</a>&copy; 2026";
    assert_eq!(scan(html), scan(html));
}
