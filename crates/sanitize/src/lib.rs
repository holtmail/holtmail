//! Making message HTML safe to show.
//!
//! [`sanitize`] removes everything that can run code, submit data or escape the reader, and by
//! default blocks remote content (tracking pixels, remote images and fonts). [`document`] wraps the
//! result in a page with a Content Security Policy, a second line of defense for the sandboxed
//! iframe the reader shows it in.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use ammonia::{Builder, UrlRelative};

/// What to do with remote images, backgrounds and styles.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Remote {
    /// Drop them. Loading them tells the sender the message was opened and from which address.
    #[default]
    Block,
    /// Point images and backgrounds at [`Options::proxy_prefix`], so the server fetches them and
    /// the reader's address stays private. Remote styles and fonts are still dropped.
    Proxy,
    /// Keep the original addresses: the reader's browser contacts the sender's servers.
    Direct,
}

/// How to treat a message's HTML.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// What to do with remote content that has no saved copy.
    pub remote: Remote,
    /// Absolute URL that a [`remote_key`] is appended to, e.g.
    /// `https://mail.example.com/api/messages/1/remote/`. Used for [`Remote::Proxy`] and for
    /// saved copies.
    pub proxy_prefix: String,
    /// [`remote_key`]s of remote images the archive keeps a copy of. These are always shown
    /// through `proxy_prefix`, whatever `remote` says, because showing them reaches no one.
    pub saved: HashSet<String>,
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

/// The key a remote URL is known by: the BLAKE3 hash of the absolute URL, in hex.
///
/// Protocol-relative URLs count as `https:`, so `//example.com/a.png` and
/// `https://example.com/a.png` share a key.
pub fn remote_key(url: &str) -> String {
    blake3::hash(absolute(url).as_bytes()).to_hex().to_string()
}

/// Absolute `http(s)` URLs of the remote images, backgrounds and style URLs in `html`, in document
/// order, each once. Protocol-relative URLs are returned as `https:`.
pub fn remote_images(html: &str) -> Vec<String> {
    let pass = Pass::new(&Options::default());
    pass.clean(html);
    let found = pass.found.lock().expect("not poisoned").clone();
    let mut seen = HashSet::new();
    found
        .into_iter()
        .filter(|url| seen.insert(url.clone()))
        .collect()
}

/// Cleans `html` from a message body.
///
/// Removes scripts, event handlers, forms, frames, objects and `javascript:` URLs; keeps tables,
/// inline styles and `<style>` blocks that email layouts depend on; rewrites `cid:` references
/// through `options.inline_parts`; treats remote URLs as `options.remote` and `options.saved` say.
/// Links open in a new tab without a referrer.
pub fn sanitize(html: &str, options: &Options) -> Sanitized {
    let pass = Pass::new(options);
    let html = pass.clean(html);
    Sanitized {
        html,
        remote_blocked: *pass.blocked.lock().expect("not poisoned"),
    }
}

#[derive(Clone)]
struct Pass {
    remote: Remote,
    prefix: Arc<str>,
    saved: Arc<HashSet<String>>,
    parts: Arc<HashMap<String, String>>,
    blocked: Arc<Mutex<bool>>,
    found: Arc<Mutex<Vec<String>>>,
}

enum Fate {
    Keep,
    Replace(String),
    Drop,
}

impl Pass {
    fn new(options: &Options) -> Self {
        Self {
            remote: options.remote,
            prefix: options.proxy_prefix.as_str().into(),
            saved: Arc::new(options.saved.clone()),
            parts: Arc::new(options.inline_parts.clone()),
            blocked: Arc::default(),
            found: Arc::default(),
        }
    }

    fn clean(&self, html: &str) -> String {
        let pass = self.clone();
        let mut builder = Builder::default();
        builder
            .add_tags(EXTRA_TAGS)
            .rm_clean_content_tags(&["style"])
            .add_generic_attributes(GENERIC_ATTRIBUTES)
            .add_tag_attributes("font", &["face", "size"])
            .add_tag_attributes("td", &["colspan", "rowspan", "nowrap", "background"])
            .add_tag_attributes("th", &["colspan", "rowspan", "nowrap", "background"])
            .add_tag_attributes("table", &["cellpadding", "cellspacing", "background"])
            .add_tag_attributes("img", &["src", "alt", "width", "height"])
            .add_url_schemes(&["cid", "data"])
            .url_relative(UrlRelative::Custom(Box::new(protocol_relative)))
            .link_rel(Some("noopener noreferrer"))
            .set_tag_attribute_value("a", "target", "_blank")
            .strip_comments(true)
            .attribute_filter(move |element, attribute, value| {
                pass.filter(element, attribute, value)
            });
        let html = builder.clean(html).to_string();
        self.style_blocks(&html)
    }

    fn fate(&self, url: &str) -> Fate {
        let url = absolute(url);
        let key = remote_key(&url);
        self.found.lock().expect("not poisoned").push(url);
        if self.saved.contains(&key) || (self.remote == Remote::Proxy && !self.prefix.is_empty()) {
            return Fate::Replace(format!("{}{key}", self.prefix));
        }
        if self.remote == Remote::Direct {
            return Fate::Keep;
        }
        *self.blocked.lock().expect("not poisoned") = true;
        Fate::Drop
    }

    fn filter<'u>(&self, element: &str, attribute: &str, value: &'u str) -> Option<Cow<'u, str>> {
        let trimmed = value.trim();
        let lower = trimmed.to_ascii_lowercase();
        match attribute {
            "src" | "background" | "srcset" => {
                if lower.starts_with("cid:") {
                    let cid = trimmed[4..].trim_matches(['<', '>']);
                    return self
                        .parts
                        .iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case(cid))
                        .map(|(_, url)| Cow::Owned(url.clone()));
                }
                if lower.starts_with("data:") {
                    return (element == "img" && lower.starts_with("data:image/"))
                        .then_some(Cow::Borrowed(value));
                }
                if is_remote(&lower) && attribute != "srcset" {
                    return match self.fate(trimmed) {
                        Fate::Keep => Some(Cow::Owned(absolute(trimmed))),
                        Fate::Replace(url) => Some(Cow::Owned(url)),
                        Fate::Drop => None,
                    };
                }
                if is_remote(&lower) && self.remote == Remote::Direct {
                    return Some(Cow::Borrowed(value));
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
            "style" => Some(self.style(value, false)),
            _ => Some(Cow::Borrowed(value)),
        }
    }

    fn style<'t>(&self, text: &'t str, block: bool) -> Cow<'t, str> {
        let lower = text.to_ascii_lowercase();
        let spans: Vec<_> = remote_url_spans(&lower).collect();
        let imports = lower.contains("@import");
        if spans.is_empty() && !imports {
            return Cow::Borrowed(text);
        }
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for (start, end, inner) in spans {
            out.push_str(&text[last..start]);
            if lower[..start].trim_end().ends_with("@import") {
                out.push_str("none");
                last = end;
                continue;
            }
            match self.fate(&text[inner.0..inner.1]) {
                Fate::Keep => out.push_str(&text[start..end]),
                Fate::Replace(url) => {
                    out.push_str("url(\"");
                    out.push_str(&url);
                    out.push_str("\")");
                }
                Fate::Drop => out.push_str("none"),
            }
            last = end;
        }
        out.push_str(&text[last..]);
        if imports && self.remote != Remote::Direct && block {
            *self.blocked.lock().expect("not poisoned") = true;
            return Cow::Owned(
                out.replace("@import", "@x-blocked-import")
                    .replace("@IMPORT", "@x-blocked-import"),
            );
        }
        Cow::Owned(out)
    }

    fn style_blocks(&self, html: &str) -> String {
        let lower = html.to_ascii_lowercase();
        let mut out = String::with_capacity(html.len());
        let mut last = 0;
        let mut from = 0;
        while let Some(open) = lower[from..].find("<style").map(|i| from + i) {
            let Some(body) = lower[open..].find('>').map(|i| open + i + 1) else {
                break;
            };
            let close = lower[body..]
                .find("</style")
                .map_or(html.len(), |i| body + i);
            out.push_str(&html[last..body]);
            out.push_str(&self.style(&html[body..close], true));
            last = close;
            from = close.max(body);
            if close == html.len() {
                break;
            }
        }
        out.push_str(&html[last..]);
        out
    }
}

fn protocol_relative(url: &str) -> Option<Cow<'_, str>> {
    url.starts_with("//").then(|| Cow::Owned(absolute(url)))
}

fn is_remote(lower: &str) -> bool {
    lower.starts_with("http:") || lower.starts_with("https:") || lower.starts_with("//")
}

fn absolute(url: &str) -> String {
    let url = url.trim();
    if url.starts_with("//") {
        format!("https:{url}")
    } else {
        url.to_owned()
    }
}

type Span = (usize, usize, (usize, usize));

fn remote_url_spans(lower: &str) -> impl Iterator<Item = Span> + '_ {
    lower.match_indices("url(").filter_map(move |(start, _)| {
        let end = lower[start..].find(')').map(|i| start + i + 1)?;
        let raw = &lower[start + 4..end - 1];
        let lead = raw.len() - raw.trim_start().len();
        let unquoted = raw.trim().trim_matches(['"', '\'']).trim();
        let offset = raw.find(unquoted).unwrap_or(lead);
        let inner = (start + 4 + offset, start + 4 + offset + unquoted.len());
        is_remote(unquoted).then_some((start, end, inner))
    })
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
            remote: Remote::Direct,
            ..Options::default()
        };
        let out = sanitize(r#"<img src="https://example.com/photo.jpg">"#, &options);
        assert!(out.html.contains("https://example.com/photo.jpg"));
        assert!(!out.remote_blocked);
    }

    const PREFIX: &str = "https://mail.example.com/api/messages/1/remote/";

    #[test]
    fn sends_remote_images_through_the_proxy() {
        let options = Options {
            remote: Remote::Proxy,
            proxy_prefix: PREFIX.into(),
            ..Options::default()
        };
        let out = sanitize(
            r#"<img src="https://example.com/photo.jpg">
               <table><tr><td background="//example.com/bg.png" style="background: url('https://example.com/a.png')">x</td></tr></table>
               <style>@import url(https://example.com/a.css); .b { background: url(https://example.com/b.png) }</style>"#,
            &options,
        );
        for url in [
            "https://example.com/photo.jpg",
            "https://example.com/bg.png",
            "https://example.com/a.png",
            "https://example.com/b.png",
        ] {
            let proxied = format!("{PREFIX}{}", remote_key(url));
            assert!(
                out.html.contains(&proxied),
                "{url} not proxied: {}",
                out.html
            );
        }
        assert!(!out.html.contains("@import"), "{}", out.html);
        assert!(!out.html.contains("\"https://example.com"), "{}", out.html);
    }

    #[test]
    fn shows_saved_copies_even_when_blocking() {
        let saved = "https://example.com/logo.png";
        let options = Options {
            proxy_prefix: PREFIX.into(),
            saved: HashSet::from([remote_key(saved)]),
            ..Options::default()
        };
        let out = sanitize(
            r#"<img src="https://example.com/logo.png"><img src="https://tracker.example.org/o.gif">"#,
            &options,
        );
        assert!(out.html.contains(&format!("{PREFIX}{}", remote_key(saved))));
        assert!(!out.html.contains("tracker.example.org"));
        assert!(out.remote_blocked);
    }

    #[test]
    fn lists_remote_images_once_in_order() {
        let urls = remote_images(
            r#"<img src="https://example.com/a.png"><img src="//example.com/b.png">
               <div style="background-image: url(https://example.com/a.png)">x</div>
               <img src="cid:part@example.com"><img src="data:image/png;base64,AAAA">
               <style>.c { background: url("https://example.org/c.png") }</style>"#,
        );
        assert_eq!(
            urls,
            [
                "https://example.com/a.png",
                "https://example.com/b.png",
                "https://example.org/c.png",
            ]
        );
    }

    #[test]
    fn keys_protocol_relative_urls_as_https() {
        assert_eq!(
            remote_key("//example.com/a.png"),
            remote_key("https://example.com/a.png")
        );
        assert_ne!(
            remote_key("http://example.com/a.png"),
            remote_key("https://example.com/a.png")
        );
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
