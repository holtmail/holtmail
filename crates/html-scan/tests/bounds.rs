//! Caps and nesting bounds of the scan.

use holtmail_html_scan::scan;
use holtmail_insight::limits::{
    MAX_ANCHORS, MAX_HTML_BYTES, MAX_LD_JSON_BLOCKS, MAX_LD_JSON_BYTES, MAX_MICRODATA_DEPTH,
    MAX_MICRODATA_PROPERTIES, MAX_SEGMENTS,
};
use holtmail_insight::{HtmlView, MicrodataValue};

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
    let view = scan(&block.repeat(MAX_LD_JSON_BLOCKS));
    assert_eq!(view.ld_json.len(), MAX_LD_JSON_BLOCKS);
    assert!(!view.truncated_structured);
    let view = scan(&block.repeat(MAX_LD_JSON_BLOCKS + 3));
    assert_eq!(view.ld_json.len(), MAX_LD_JSON_BLOCKS);
    assert!(view.truncated_structured);

    let big = format!(
        "<script type=\"Application/LD+JSON; charset=utf-8\">\"{}\"</script>{block}",
        "x".repeat(MAX_LD_JSON_BYTES)
    );
    let view = scan(&big);
    assert_eq!(view.ld_json, ["{\"@type\":\"Order\"}"]);
    assert!(view.truncated_structured);

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

fn fill(size: usize, unit: impl Fn(usize) -> String) -> String {
    let mut out = String::with_capacity(size + 256);
    let mut n = 0;
    while out.len() < size {
        out.push_str(&unit(n));
        n += 1;
    }
    out
}

fn worst_cases(size: usize) -> Vec<(&'static str, String)> {
    vec![
        ("nested-div-b", "<div><b>".repeat(size / 8)),
        (
            "nested-formatting",
            fill(size, |n| {
                ["<b>", "<i>", "<u>", "<em>", "<font>"][n % 5].to_owned()
            }),
        ),
        (
            "misnested-formatting",
            fill(size, |n| format!("<b><i>x{n}</b></i><p>")),
        ),
        (
            "deep-tables",
            fill(size, |n| format!("<table><tr><td>cell {n}")),
        ),
        (
            "table-soup",
            fill(size, |_| {
                "<table><b><td><a href=\"https://example.com/\">x".to_owned()
            }),
        ),
        ("deep-lists", fill(size, |n| format!("<ul><li><p>{n}"))),
        (
            "foreign",
            fill(size, |n| format!("<svg><foreignObject><math><mi>{n}")),
        ),
        (
            "many-anchors",
            fill(size, |n| {
                format!(
                    "<a href=\"https://example.com/v?c={n}\">verify {n}</a> <strong>{n:06}</strong>\n"
                )
            }),
        ),
        (
            "unclosed-anchors",
            fill(size, |n| {
                format!("<a href=\"https://example.com/{n}\"><b>{n}")
            }),
        ),
        (
            "many-attributes",
            fill(size, |n| {
                format!(
                    "<span data-a{n}=\"1\" title=\"t\" class=\"c\" id=\"i{n}\" lang=\"en\">{n}</span>"
                )
            }),
        ),
        (
            "microdata-fan-in",
            fill(size, |n| {
                if n == 0 {
                    format!(
                        "<div id=\"big\">{}</div>",
                        "<span itemprop=\"p\">x</span>".repeat(3_000)
                    )
                } else {
                    "<i itemscope itemref=\"big big b2\"></i>".to_owned()
                }
            }),
        ),
        (
            "microdata-nested-items",
            fill(size, |n| {
                format!(
                    "<div itemprop=\"c\" itemscope itemtype=\"https://schema.org/T\" id=\"d{n}\">t{n} "
                )
            }),
        ),
        (
            "microdata-many-ids",
            fill(size, |n| {
                if n % 64 == 63 {
                    let refs = (n - 63..n).map(|i| format!("i{i} ")).collect::<String>();
                    format!("<b itemscope itemref=\"{refs}\"></b>")
                } else {
                    format!(
                        "<div id=\"i{n}\"><span itemprop=\"p\">v{n}</span><span itemprop=\"q\">"
                    )
                }
            }),
        ),
        (
            "microdata-text-props",
            fill(size, |n| {
                format!("<span itemprop=\"t\">w{n} <em>&amp; </em>")
            }),
        ),
        (
            "long-style",
            fill(size, |n| {
                format!(
                    "<p style=\"{}font-weight:bold\">{n}</p>",
                    "color:red;margin:0;".repeat(200)
                )
            }),
        ),
    ]
}

#[test]
fn worst_cases_stay_bounded() {
    for (name, html) in worst_cases(64 * 1024) {
        let view = scan(&html);
        assert!(view.anchors.len() <= MAX_ANCHORS, "{name}");
        assert!(view.segments.len() <= MAX_SEGMENTS, "{name}");
        assert_microdata_bounded(name, &view);
    }
}

fn assert_microdata_bounded(name: &str, view: &HtmlView) {
    let total: usize = view.microdata.iter().map(|i| i.properties.len()).sum();
    assert!(total <= MAX_MICRODATA_PROPERTIES, "{name}: {total}");
    assert!(view.microdata.len() <= MAX_MICRODATA_PROPERTIES, "{name}");
    let mut depth = vec![0usize; view.microdata.len()];
    for (i, item) in view.microdata.iter().enumerate() {
        if item.top_level {
            depth[i] = depth[i].max(1);
        }
        for property in &item.properties {
            if let MicrodataValue::Item(j) = property.value {
                assert!(j < view.microdata.len(), "{name}");
                if j > i {
                    depth[j] = depth[j].max(depth[i] + 1);
                }
            }
        }
    }
    assert!(
        depth.iter().all(|d| *d <= MAX_MICRODATA_DEPTH),
        "{name}: {:?}",
        depth.iter().max()
    );
}

#[test]
fn microdata_bombs_stay_bounded_at_2_mib() {
    for (name, html) in worst_cases(2 * 1024 * 1024)
        .into_iter()
        .filter(|(name, _)| name.starts_with("microdata"))
    {
        let view = scan(&html);
        assert_microdata_bounded(name, &view);
        assert!(view.truncated_structured, "{name}");
    }
}

#[cfg(not(debug_assertions))]
mod timing {
    use std::time::{Duration, Instant};

    use html5ever::TokenizerResult;
    use html5ever::tendril::StrTendril;
    use html5ever::tokenizer::{
        BufferQueue, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
    };

    use super::{scan, worst_cases};

    fn best_of_5(f: impl Fn()) -> Duration {
        (0..5)
            .map(|_| {
                let started = Instant::now();
                f();
                started.elapsed()
            })
            .min()
            .unwrap_or_default()
    }

    fn tokenize(html: &str) {
        struct Discard;
        impl TokenSink for Discard {
            type Handle = ();
            fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
                std::hint::black_box(token);
                TokenSinkResult::Continue
            }
        }
        let tokenizer = Tokenizer::new(Discard, TokenizerOpts::default());
        let queue = BufferQueue::default();
        queue.push_back(StrTendril::from_slice(html));
        while !matches!(tokenizer.feed(&queue), TokenizerResult::Done) {}
        tokenizer.end();
    }

    /// The stage-A budget is 40 ms. A loaded machine slows html5ever's tokenizer as much as the scan,
    /// so the limit is 40 ms or three times the bare tokenizer on the same input, whichever is
    /// larger: quadratic work still fails by a wide margin.
    #[test]
    fn worst_cases_scan_within_the_stage_a_budget() {
        let mut cases = worst_cases(2 * 1024 * 1024);
        cases.insert(0, ("nested-div-b-1.6mb", "<div><b>".repeat(200_000)));
        for (name, html) in cases {
            let took = best_of_5(|| {
                std::hint::black_box(scan(&html));
            });
            let floor = best_of_5(|| tokenize(&html));
            let budget = Duration::from_millis(40).max(floor * 3);
            println!(
                "{name}: {} bytes, scan {took:?}, tokenizer {floor:?}",
                html.len()
            );
            assert!(took <= budget, "{name}: {took:?} over {budget:?}");
        }
    }
}
