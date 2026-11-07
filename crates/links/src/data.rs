use std::sync::LazyLock;

use serde::Deserialize;

pub(crate) const TRACKING_PARAMS: &str = include_str!("../data/tracking-params.toml");
pub(crate) const TRACKERS: &str = include_str!("../data/trackers.toml");
pub(crate) const PSL: &str = include_str!("../data/public_suffix_list.dat");

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrackingParams {
    pub(crate) prefixes: Vec<String>,
    pub(crate) exact: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Tracker {
    pub(crate) name: String,
    pub(crate) host: String,
    pub(crate) path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Trackers {
    pub(crate) tracker: Vec<Tracker>,
}

pub(crate) fn parse_tracking_params() -> Result<TrackingParams, String> {
    let mut params: TrackingParams = toml::from_str(TRACKING_PARAMS).map_err(|e| e.to_string())?;
    for name in params.prefixes.iter_mut().chain(params.exact.iter_mut()) {
        *name = name.to_ascii_lowercase();
    }
    Ok(params)
}

pub(crate) fn parse_trackers() -> Result<Trackers, String> {
    let mut trackers: Trackers = toml::from_str(TRACKERS).map_err(|e| e.to_string())?;
    for tracker in &mut trackers.tracker {
        tracker.host = tracker.host.to_ascii_lowercase();
    }
    Ok(trackers)
}

pub(crate) static TRACKING_PARAMS_LIST: LazyLock<TrackingParams> =
    LazyLock::new(|| parse_tracking_params().unwrap_or_default());

pub(crate) static TRACKER_LIST: LazyLock<Trackers> =
    LazyLock::new(|| parse_trackers().unwrap_or_default());

pub(crate) static PSL_LIST: LazyLock<publicsuffix::List> =
    LazyLock::new(|| publicsuffix::List::from_bytes(PSL.as_bytes()).unwrap_or_default());

fn hex12(hash: blake3::Hash) -> String {
    hash.to_hex().chars().take(12).collect()
}

static VERSION_STRING: LazyLock<String> = LazyLock::new(|| {
    let mut hasher = blake3::Hasher::new();
    hasher.update(TRACKING_PARAMS.as_bytes());
    hasher.update(&[0]);
    hasher.update(TRACKERS.as_bytes());
    format!("{}+{}", crate::VERSION, hex12(hasher.finalize()))
});

static PSL_VERSION: LazyLock<String> = LazyLock::new(|| hex12(blake3::hash(PSL.as_bytes())));

/// The full version of everything that decides [`unwrap`](crate::unwrap),
/// [`strip_tracking`](crate::strip_tracking) and [`tracker`](crate::tracker):
/// `"<VERSION>+<12 hex>"`, where the hex is the start of the BLAKE3 hash of the bundled
/// tracking-parameter and tracker lists.
///
/// It is stable for a given build and changes whenever [`VERSION`](crate::VERSION) or either data
/// file changes, so it belongs in fingerprints of data derived at extraction time. It does not
/// cover the Public Suffix List; see [`psl_version`].
pub fn version() -> &'static str {
    &VERSION_STRING
}

/// The first 12 hex digits of the BLAKE3 hash of the bundled Public Suffix List.
///
/// It changes exactly when the bundled list changes and covers
/// [`registrable_domain`](crate::registrable_domain). It is meant for fingerprints of data
/// derived after extraction.
pub fn psl_version() -> &'static str {
    &PSL_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_files_parse() {
        let params = parse_tracking_params().expect("tracking params");
        assert!(params.prefixes.iter().any(|p| p == "utm_"));
        assert!(params.exact.iter().any(|p| p == "gclid"));
        let trackers = parse_trackers().expect("trackers");
        assert!(trackers.tracker.len() >= 10);
        for tracker in &trackers.tracker {
            let suffix = tracker.host.strip_prefix("*.").unwrap_or(&tracker.host);
            assert!(
                !suffix.is_empty() && !suffix.contains('*'),
                "{}",
                tracker.host
            );
            if let Some(path) = &tracker.path {
                assert!(path.starts_with('/') && !path.ends_with('/'), "{path}");
            }
        }
        let psl = publicsuffix::List::from_bytes(PSL.as_bytes()).expect("psl");
        assert!(!psl.is_empty());
    }

    #[test]
    fn versions_have_their_shape() {
        let v = version();
        assert!(v.starts_with(&format!("{}+", crate::VERSION)));
        assert_eq!(v.len(), crate::VERSION.len() + 13);
        assert_eq!(psl_version().len(), 12);
        assert!(psl_version().chars().all(|c| c.is_ascii_hexdigit()));
    }
}
