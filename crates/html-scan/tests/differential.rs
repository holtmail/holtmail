//! The scan reports nothing that holtmail-sanitize removes.

use std::collections::BTreeSet;

use holtmail_html_scan::scan;
use holtmail_sanitize::{Options, sanitize};

fn survives_sanitizing(html: &str) {
    let view = scan(html);
    let clean = sanitize(html, &Options::default()).html;
    let again = scan(&clean);
    let hrefs: BTreeSet<&str> = again.anchors.iter().map(|a| a.href.as_str()).collect();
    for anchor in &view.anchors {
        assert!(
            hrefs.contains(anchor.href.as_str()),
            "href {:?} lost by sanitizing {html:?}",
            anchor.href
        );
    }
    let text = again
        .segments
        .iter()
        .flat_map(|s| s.text.chars())
        .filter(|c| !c.is_whitespace())
        .collect::<String>();
    for segment in &view.segments {
        for word in segment.text.split_whitespace() {
            assert!(
                text.contains(word),
                "word {word:?} lost by sanitizing {html:?}"
            );
        }
    }
}

#[test]
fn fixtures_survive_sanitizing() {
    let dir = format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"));
    let mut count = 0;
    for entry in std::fs::read_dir(dir).expect("fixtures") {
        let path = entry.expect("entry").path();
        if path.extension().is_some_and(|e| e == "html") {
            survives_sanitizing(&std::fs::read_to_string(&path).expect("fixture"));
            count += 1;
        }
    }
    assert!(count >= 20);
}

#[test]
fn tricky_markup_survives_sanitizing() {
    for html in [
        "<a href=\"javascript:alert(1)\">x</a><a href=\"https://example.com/a\">y</a>",
        "<a href=\"data:text/html,x\">x</a><a href=\"cid:part1\">c</a><a href=\"/relative\">r</a>",
        "<a href=\"//example.com/p\">p</a><a href=\"mailto:help@example.com\">m</a>",
        "<a href=\"https://exa mple.com/\">bad</a><a href=\"http://example.org/?a=1&amp;b=2\">q</a>",
        "<text><div>a</text>b</div>c",
        "<div><g>inside</div>after",
        "<svg><desc><p>desc text</p></desc></svg>after svg",
        "<math><annotation-xml encoding=\"text/html\"><p>ann</p></annotation-xml></math>after",
        "<svg><style>a</svg><p>b</p>",
        "<template><a href=\"https://example.com/t\">t</a></div></template>outside",
        "<table>fostered<tr><td>cell</td></tr></table>",
        "<select><option>opt</option></select>after select",
        "<object><p>fallback</p></object>after object",
        "<b><p>x</b>y</p>",
        "<p>a<p>b<li>c<li>d<dd>e<dt>f",
        "<noscript><a href=\"https://example.com/n\">n</a></noscript>after",
        "<!-- <a href=\"https://example.com/c\">c</a> -->after comment",
        "<![CDATA[<a href=\"https://example.com/cd\">cd</a>]]>after cdata",
        "<svg><![CDATA[cdata in svg]]></svg>after",
        "<p>caf&eacute; &nbsp;&#8203;zero&#xad;width\u{0}null</p>",
        "<form><input value=\"v\"><button>Press</button><textarea>ta</textarea></form>",
        "<frameset><frame src=\"x\"></frameset>text",
        "<image src=\"x\">after image",
        "<a href=\"https://example.com/1\"><table><tr><td><a href=\"https://example.com/2\">2</a></td></tr></table></a>",
    ] {
        survives_sanitizing(html);
    }
}

#[test]
fn generated_tag_soup_survives_sanitizing() {
    const PIECES: &[&str] = &[
        "<p>",
        "</p>",
        "<div>",
        "</div>",
        "<b>",
        "</b>",
        "<a href=\"https://example.com/a\">",
        "<a href=\"mailto:x@example.org\">",
        "</a>",
        "<table>",
        "<tr>",
        "<td>",
        "</td>",
        "</tr>",
        "</table>",
        "<li>",
        "<ul>",
        "</ul>",
        "<svg>",
        "</svg>",
        "<math>",
        "</math>",
        "<mi>",
        "</mi>",
        "<foreignObject>",
        "<desc>",
        "<template>",
        "</template>",
        "<script>",
        "</script>",
        "<style>",
        "</style>",
        "<title>",
        "</title>",
        "<textarea>",
        "</textarea>",
        "<noscript>",
        "</noscript>",
        "<xmp>",
        "</xmp>",
        "<iframe>",
        "</iframe>",
        "<select>",
        "</select>",
        "<option>",
        "<text>",
        "</text>",
        "<g>",
        "</g>",
        "<span>",
        "</span>",
        "<font color=red>",
        "</font>",
        "<object>",
        "</object>",
        "<br>",
        "</br>",
        "<h1>",
        "</h1>",
        "<!--",
        "-->",
        "<![CDATA[",
        "]]>",
        "<caption>",
        "<colgroup>",
        "<button>",
        "</button>",
        "<form>",
        "</form>",
        "<dd>",
        "<dt>",
        "<frameset>",
        "<image>",
        "<body>",
        "<html>",
        "alpha",
        "beta ",
        " gamma",
        "&amp;",
        "&lt;",
        "©",
        "<p hidden>",
        "<div style=\"display:none\">",
        "<span aria-hidden=true>",
        "<annotation-xml encoding=\"text/html\">",
        "</annotation-xml>",
    ];
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..20_000 {
        let len = 1 + next() % 40;
        let html: String = (0..len)
            .map(|_| PIECES[(next() % PIECES.len() as u64) as usize])
            .collect();
        survives_sanitizing(&html);
    }
}
