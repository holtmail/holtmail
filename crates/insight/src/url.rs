use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::InsightError;
use crate::limits::{MAX_DESTINATION_BYTES, MAX_URL_BYTES};

/// The schemes a [`Url`] may have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UrlScheme {
    /// `https:`
    Https,
    /// `http:`
    Http,
    /// `mailto:`
    Mailto,
}

/// A URL an insight may carry.
///
/// Guarantees, checked by [`Url::parse`] and again when read from JSON:
///
/// - the scheme is `https`, `http` or `mailto`;
/// - an `http(s)` URL has a host and no userinfo, and its host is stored as an A-label (IDNA,
///   UTS 46), so look-alike Unicode hosts are visible as `xn--` labels;
/// - a `mailto` URL has a non-empty address part and no authority;
/// - the input and the normalised form are both at most [`MAX_URL_BYTES`] bytes. Longer URLs are
///   rejected, never cut.
///
/// Equality, ordering and hashing follow the normalised string.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Url(url::Url);

impl Url {
    /// Reads and normalises `input`, refusing anything outside the rules above.
    pub fn parse(input: &str) -> Result<Self, InsightError> {
        if input.len() > MAX_URL_BYTES {
            return Err(InsightError::Decode("url too long"));
        }
        let parsed = url::Url::parse(input).map_err(|_| InsightError::Decode("url syntax"))?;
        match parsed.scheme() {
            "https" | "http" => {
                if parsed.host().is_none() {
                    return Err(InsightError::Decode("url without host"));
                }
                if !parsed.username().is_empty() || parsed.password().is_some() {
                    return Err(InsightError::Decode("url with userinfo"));
                }
            }
            "mailto" => {
                if parsed.host().is_some() || parsed.path().is_empty() {
                    return Err(InsightError::Decode("mailto without address"));
                }
            }
            _ => return Err(InsightError::Decode("url scheme")),
        }
        if parsed.as_str().len() > MAX_URL_BYTES {
            return Err(InsightError::Decode("url too long"));
        }
        Ok(Self(parsed))
    }

    /// The normalised URL.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// The scheme.
    pub fn scheme(&self) -> UrlScheme {
        match self.0.scheme() {
            "https" => UrlScheme::Https,
            "http" => UrlScheme::Http,
            _ => UrlScheme::Mailto,
        }
    }

    /// The host as an A-label or an IP literal; `None` for `mailto`.
    pub fn host(&self) -> Option<&str> {
        self.0.host_str()
    }

    /// Whether this URL may be an action's `destination`: at most [`MAX_DESTINATION_BYTES`] bytes.
    pub fn fits_destination(&self) -> bool {
        self.as_str().len() <= MAX_DESTINATION_BYTES
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Url({:?})", self.as_str())
    }
}

impl std::str::FromStr for Url {
    type Err = InsightError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl Serialize for Url {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Url {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_allowed_schemes() {
        assert_eq!(
            Url::parse("https://example.com/verify?t=1")
                .unwrap()
                .scheme(),
            UrlScheme::Https
        );
        assert_eq!(
            Url::parse("http://example.com/").unwrap().scheme(),
            UrlScheme::Http
        );
        let mailto = Url::parse("mailto:leave@example.com?subject=unsubscribe").unwrap();
        assert_eq!(mailto.scheme(), UrlScheme::Mailto);
        assert_eq!(mailto.host(), None);
    }

    #[test]
    fn refuses_other_schemes_and_userinfo() {
        for bad in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,hi",
            "ftp://example.com/",
            "https://ada@example.com/",
            "https://ada:secret@example.com/",
            "mailto:",
            "mailto://example.com",
            "not a url",
            "",
        ] {
            assert!(Url::parse(bad).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn stores_hosts_as_a_labels() {
        let url = Url::parse("https://bücher.example/").unwrap();
        assert_eq!(url.host(), Some("xn--bcher-kva.example"));
        assert_eq!(url.as_str(), "https://xn--bcher-kva.example/");
    }

    #[test]
    fn rejects_long_urls_instead_of_cutting() {
        let path = "a".repeat(MAX_URL_BYTES);
        assert!(Url::parse(&format!("https://example.com/{path}")).is_err());
        let fits = format!("https://example.com/{}", "a".repeat(MAX_DESTINATION_BYTES));
        assert!(!Url::parse(&fits).unwrap().fits_destination());
        assert!(
            Url::parse("https://example.com/")
                .unwrap()
                .fits_destination()
        );
    }

    #[test]
    fn json_reading_checks_the_rules() {
        let url: Url = serde_json::from_str("\"https://example.com/a\"").unwrap();
        assert_eq!(
            serde_json::to_string(&url).unwrap(),
            "\"https://example.com/a\""
        );
        assert!(serde_json::from_str::<Url>("\"javascript:alert(1)\"").is_err());
    }

    #[test]
    fn errors_carry_no_content() {
        let err = Url::parse("https://ada:hunter2@example.com/").unwrap_err();
        assert!(!err.to_string().contains("hunter2"));
    }
}
