//! Making message HTML safe to show.
//!
//! [`sanitize`] removes everything that can run code, submit data or escape the reader, and by
//! default blocks remote content (tracking pixels, remote images and fonts). [`document`] wraps the
//! result in a page with a Content Security Policy, a second line of defense for the sandboxed
//! iframe the reader shows it in.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ammonia::{Builder, UrlRelative};

/// How to treat a message's HTML.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Keep `http(s)` images, backgrounds and styles. Off by default: loading them tells the sender
    /// the message was opened.
    pub allow_remote: bool,
    /// `Content-ID` (without angle brackets) to the absolute URL that serves that inline part.
    /// Relative URLs are dropped like every other relative URL in a message.
    pub inline_parts: HashMap<String, String>,
}

/// Sanitized HTML.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sanitized {
    /// HTML safe to put inside [`document`].
    pub html: String,
    /// The message referenced remote content that was removed.
    pub remote_blocked: bool,
}

const GENERIC_ATTRIBUTES: &[&str] = &[
    "align", "bgcolor", "border", "class", "color", "dir", "height", "style", "valign", "width",
];
const EXTRA_TAGS: &[&str] = &["center", "font", "style"];

/// Cleans `html` from a message body.
///
/// Removes scripts, event handlers, forms, frames, objects and `javascript:` URLs; keeps tables,
/// inline styles and `<style>` blocks that email layouts depend on; rewrites `cid:` references
/// through `options.inline_parts`; drops remote URLs unless `options.allow_remote`. Links open in a
/// new tab without a referrer.
pub fn sanitize(html: &str, options: &Options) -> Sanitized {
    let blocked = Arc::new(AtomicBool::new(false));
    let filter_blocked = blocked.clone();
    let allow_remote = options.allow_remote;
    let parts = options.inline_parts.clone();

    let mut builder = Builder::default();
    builder
        .add_tags(EXTRA_TAGS)
        .rm_clean_content_tags(&["style"])
        .add_generic_attributes(GENERIC_ATTRIBUTES)
        .add_tag_attributes("font", &["face", "size"])
        .add_tag_attributes("td", &["colspan", "rowspan", "nowrap"])
        .add_tag_attributes("th", &["colspan", "rowspan", "nowrap"])
        .add_tag_attributes("table", &["cellpadding", "cellspacing"])
        .add_tag_attributes("img", &["src", "alt", "width", "height"])
        .add_url_schemes(&["cid", "data"])
        .url_relative(UrlRelative::Deny)
        .link_rel(Some("noopener noreferrer"))
        .set_tag_attribute_value("a", "target", "_blank")
        .strip_comments(true)
        .attribute_filter(move |element, attribute, value| {
            filter(
                element,
                attribute,
                value,
                allow_remote,
                &parts,
                &filter_blocked,
            )
        });
    let mut html = builder.clean(html).to_string();
    if !allow_remote && style_has_remote(&html) {
        html = strip_remote_style_urls(&html);
        blocked.store(true, Ordering::Relaxed);
    }
    Sanitized {
        html,
        remote_blocked: blocked.load(Ordering::Relaxed),
    }
}

fn filter<'u>(
    element: &str,
    attribute: &str,
    value: &'u str,
    allow_remote: bool,
    parts: &HashMap<String, String>,
    blocked: &AtomicBool,
) -> Option<Cow<'u, str>> {
    let lower = value.trim_start().to_ascii_lowercase();
    match attribute {
        "src" | "background" | "srcset" => {
            if lower.starts_with("cid:") {
                let cid = value.trim_start()[4..].trim_matches(['<', '>']);
                return parts
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case(cid))
                    .map(|(_, url)| Cow::Owned(url.clone()));
            }
            if lower.starts_with("data:") {
                return (element == "img" && lower.starts_with("data:image/"))
                    .then_some(Cow::Borrowed(value));
            }
            if lower.starts_with("http:") || lower.starts_with("https:") || lower.starts_with("//")
            {
                if allow_remote {
                    return Some(Cow::Borrowed(value));
                }
                blocked.store(true, Ordering::Relaxed);
                return None;
            }
            None
        }
        "href" => {
            if lower.starts_with("data:") || lower.starts_with("cid:") {
                None
            } else {
                Some(Cow::Borrowed(value))
            }
        }
        "style" => {
            if !allow_remote && style_has_remote(&lower) {
                blocked.store(true, Ordering::Relaxed);
                Some(Cow::Owned(strip_remote_style_urls(value)))
            } else {
                Some(Cow::Borrowed(value))
            }
        }
        _ => Some(Cow::Borrowed(value)),
    }
}

fn style_has_remote(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("@import") || remote_url_spans(&lower).next().is_some()
}

fn remote_url_spans(lower: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    lower.match_indices("url(").filter_map(move |(start, _)| {
        let end = lower[start..].find(')').map(|i| start + i + 1)?;
        let inner = lower[start + 4..end - 1]
            .trim()
            .trim_matches(['"', '\''])
            .trim();
        let remote =
            inner.starts_with("http:") || inner.starts_with("https:") || inner.starts_with("//");
        remote.then_some((start, end))
    })
}

fn strip_remote_style_urls(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (start, end) in remote_url_spans(&lower) {
        out.push_str(&text[last..start]);
        out.push_str("none");
        last = end;
    }
    out.push_str(&text[last..]);
    out.replace("@import", "@x-blocked-import")
        .replace("@IMPORT", "@x-blocked-import")
}

/// A full HTML page around sanitized `body`, with a Content Security Policy that forbids scripts,
/// forms, frames and plugins, and allows remote images, fonts and styles only when
/// `allow_remote`. `inline_origin` is where inline parts are served from, e.g. `https://mail.example.com`.
pub fn document(body: &str, allow_remote: bool, inline_origin: &str) -> String {
    let remote = if allow_remote { " https: http:" } else { "" };
    let csp = format!(
        "default-src 'none'; img-src {inline_origin} data: cid:{remote}; \
         style-src 'unsafe-inline'{remote}; font-src data:{remote}; \
         form-action 'none'; base-uri 'none'; frame-ancestors 'self'"
    );
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">\
         <meta name=\"referrer\" content=\"no-referrer\">\
         <base target=\"_blank\"></head><body>{body}</body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(html: &str) -> Sanitized {
        sanitize(html, &Options::default())
    }

    #[test]
    fn removes_scripts_handlers_and_forms() {
        let out = clean(
            r#"<p onclick="steal()">Hi<script>alert(1)</script></p>
               <form action="https://example.com/x"><input name="password"></form>
               <a href="javascript:alert(1)">x</a><iframe src="https://example.com"></iframe>"#,
        );
        for bad in [
            "script",
            "onclick",
            "<form",
            "<input",
            "javascript:",
            "iframe",
        ] {
            assert!(!out.html.contains(bad), "{bad} survived: {}", out.html);
        }
        assert!(out.html.contains("Hi"));
    }

    #[test]
    fn keeps_email_layout() {
        let out = clean(
            r##"<style>.title { color: #333 }</style>
               <table width="600" cellpadding="0" bgcolor="#ffffff"><tr><td align="center" style="padding: 8px" class="title">Hello</td></tr></table>"##,
        );
        for kept in [
            "<style>",
            "width=\"600\"",
            "cellpadding",
            "bgcolor",
            "align=\"center\"",
            "padding: 8px",
            "class=\"title\"",
        ] {
            assert!(out.html.contains(kept), "{kept} was removed: {}", out.html);
        }
        assert!(!out.remote_blocked);
    }

    #[test]
    fn blocks_remote_content_by_default() {
        let out = clean(
            r#"<img src="https://tracker.example.com/open.gif" width="1">
               <div style="background: url('https://example.com/bg.png') no-repeat">x</div>
               <style>@import url(https://example.com/a.css); .a { background: url(https://example.com/b.png) }</style>"#,
        );
        assert!(out.remote_blocked);
        assert!(!out.html.contains("https://example.com"), "{}", out.html);
        assert!(!out.html.contains("tracker.example.com"), "{}", out.html);
    }

    #[test]
    fn allows_remote_content_on_request() {
        let options = Options {
            allow_remote: true,
            ..Options::default()
        };
        let out = sanitize(r#"<img src="https://example.com/photo.jpg">"#, &options);
        assert!(out.html.contains("https://example.com/photo.jpg"));
        assert!(!out.remote_blocked);
    }

    #[test]
    fn rewrites_inline_parts() {
        let options = Options {
            inline_parts: HashMap::from([(
                "logo@example.com".to_string(),
                "https://mail.example.com/api/messages/1/parts/0".to_string(),
            )]),
            ..Options::default()
        };
        let out = sanitize(
            r#"<img src="cid:logo@example.com"><img src="cid:missing@example.com">"#,
            &options,
        );
        assert!(
            out.html
                .contains(r#"src="https://mail.example.com/api/messages/1/parts/0""#),
            "{}",
            out.html
        );
        assert!(!out.html.contains("missing@example.com"));
    }

    #[test]
    fn keeps_data_images_only_in_img() {
        let out = clean(
            r#"<img src="data:image/png;base64,AAAA"><a href="data:text/html,<b>x</b>">x</a>"#,
        );
        assert!(out.html.contains("data:image/png"));
        assert!(!out.html.contains("data:text/html"));
    }

    #[test]
    fn links_open_safely() {
        let out = clean(r#"<a href="https://example.com">site</a>"#);
        assert!(out.html.contains("target=\"_blank\""));
        assert!(out.html.contains("noopener noreferrer"));
    }

    #[test]
    fn document_sets_a_strict_policy() {
        let page = document("<p>x</p>", false, "https://mail.example.com");
        assert!(page.contains("default-src 'none'"));
        assert!(page.contains("img-src https://mail.example.com data: cid:;"));
        assert!(!page.contains("https: http:"));
        assert!(document("", true, "").contains("https: http:"));
    }
}
