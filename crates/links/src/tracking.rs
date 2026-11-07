use crate::data::{TRACKER_LIST, TRACKING_PARAMS_LIST};
use crate::unwrap::parse_web;

/// Removes tracking query parameters from an `http` or `https` URL.
///
/// A parameter is removed when its form-decoded name, compared case-insensitively, is on the
/// bundled list (`data/tracking-params.toml`: exact names such as `gclid`, `fbclid`, `mc_eid`,
/// `_hsenc`, `mkt_tok`, and prefixes such as `utm_`).
///
/// Guarantees: the other parameters keep their order and their exact bytes; the fragment is kept;
/// when nothing is removed, or the input is not an `http(s)` URL without userinfo that parses, the
/// input is returned unchanged. Deterministic for a given [`version`](crate::version); never
/// panics.
pub fn strip_tracking(url: &str) -> String {
    let Some(mut parsed) = parse_web(url) else {
        return url.to_owned();
    };
    let Some(query) = parsed.query() else {
        return url.to_owned();
    };
    let kept: Vec<&str> = query.split('&').filter(|pair| !is_tracking(pair)).collect();
    if kept.len() == query.split('&').count() {
        return url.to_owned();
    }
    let kept = kept.join("&");
    parsed.set_query((!kept.is_empty()).then_some(kept.as_str()));
    parsed.into()
}

fn is_tracking(pair: &str) -> bool {
    let Some((name, _)) = url::form_urlencoded::parse(pair.as_bytes()).next() else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    let list = &*TRACKING_PARAMS_LIST;
    list.exact.contains(&name) || list.prefixes.iter().any(|p| name.starts_with(p.as_str()))
}

/// The name of the opaque ESP click tracker that `url` goes through, if any.
///
/// The host must equal a listed host, or end in a listed `*.` suffix on a label boundary with at
/// least one label in front; when the entry has a path, the URL's path must start with it on a
/// segment boundary. The list is `data/trackers.toml` (for example SendGrid `*.ct.sendgrid.net`
/// `/ls/click`, Mailchimp `*.list-manage.com` `/track/click`).
///
/// Guarantees: `None` for anything that is not an `http(s)` URL without userinfo; never matches
/// by substring; deterministic for a given [`version`](crate::version); never panics.
pub fn tracker(url: &str) -> Option<&'static str> {
    let parsed = parse_web(url)?;
    let url::Host::Domain(host) = parsed.host()? else {
        return None;
    };
    let path = parsed.path();
    let list: &'static crate::data::Trackers = &TRACKER_LIST;
    list.tracker
        .iter()
        .find(|t| {
            host_matches(host, &t.host) && t.path.as_deref().is_none_or(|p| path_matches(path, p))
        })
        .map(|t| t.name.as_str())
}

fn host_matches(host: &str, pattern: &str) -> bool {
    match pattern.strip_prefix("*.") {
        Some(suffix) => host
            .strip_suffix(suffix)
            .and_then(|front| front.strip_suffix('.'))
            .is_some_and(|front| !front.is_empty() && !front.ends_with('.')),
        None => host == pattern,
    }
}

fn path_matches(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_listed_parameters_in_order() {
        assert_eq!(
            strip_tracking(
                "https://example.com/a?utm_source=news&id=7&UTM_Medium=mail&gclid=x&b=c%20d&fbclid=y#top"
            ),
            "https://example.com/a?id=7&b=c%20d#top"
        );
        assert_eq!(
            strip_tracking("https://example.com/?utm_campaign=x&mc_eid=1&_hsenc=2"),
            "https://example.com/"
        );
        assert_eq!(
            strip_tracking("https://example.com/?utm%5Fsource=x&k=v"),
            "https://example.com/?k=v"
        );
    }

    #[test]
    fn identity_when_nothing_matches() {
        for url in [
            "https://example.com/a?id=7&utm=1&source=x",
            "https://example.com/a",
            "HTTPS://EXAMPLE.COM/a?id=7",
            "mailto:a@example.com?utm_source=x",
            "https://user@example.com/?utm_source=x",
            "not a url ?utm_source=x",
            "",
        ] {
            assert_eq!(strip_tracking(url), url, "{url}");
        }
    }

    #[test]
    fn trackers_hit() {
        for (url, name) in [
            (
                "https://u123456.ct.sendgrid.net/ls/click?upn=abc",
                "SendGrid",
            ),
            (
                "https://example.us1.list-manage.com/track/click?u=1&id=2",
                "Mailchimp",
            ),
            ("https://mandrillapp.com/track/click/123", "Mandrill"),
            ("https://d1abc.na1.hubspotlinks.com/Ctc/x", "HubSpot"),
            ("https://e.customeriomail.com/e/c/abc", "Customer.io"),
            (
                "https://click.exacttarget.com/?qs=abc",
                "Salesforce Marketing Cloud",
            ),
            (
                "https://cl.s7.exct.net/?qs=abc",
                "Salesforce Marketing Cloud",
            ),
            (
                "https://abc.r.us-east-1.awstrack.me/L0/https:%2F%2Fexample.com",
                "Amazon SES",
            ),
            ("https://click.pstmrk.it/3s/example.com", "Postmark"),
            ("https://go.sparkpostmail.com/f/a/b", "SparkPost"),
            ("HTTPS://U1.CT.SENDGRID.NET/ls/click", "SendGrid"),
        ] {
            assert_eq!(tracker(url), Some(name), "{url}");
        }
    }

    #[test]
    fn trackers_near_misses() {
        for url in [
            "https://ct.sendgrid.net/ls/click?upn=abc",
            "https://u1.ct.sendgrid.net/ls/clicker",
            "https://u1.ct.sendgrid.net/wf/open",
            "https://u1.ct.sendgrid.net.evil.example/ls/click",
            "https://u1evilct.sendgrid.net/ls/click",
            "https://list-manage.com/track/click",
            "https://example.com/track/click?u=1",
            "https://evil.example/?u=https://x.list-manage.com/track/click",
            "https://user@u1.ct.sendgrid.net/ls/click",
            "ftp://u1.ct.sendgrid.net/ls/click",
            "https://hubspotlinks.com/x",
            "https://notexacttarget.com/",
            "not a url",
        ] {
            assert_eq!(tracker(url), None, "{url}");
        }
    }
}
