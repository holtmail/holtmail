use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};

use holtmail_insight::{CarrierId, Url, UrlScheme};
use serde::Deserialize;

use crate::check::SCHEMES;
use crate::doc::fold;

pub(crate) const SOURCE: &str = include_str!("../data/carriers.toml");

const MAX_PATTERN_CHARS: usize = 32;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    scores: Scores,
    context: Context,
    keywords: BTreeMap<String, Vec<String>>,
    negative: Negative,
    carriers: BTreeMap<String, CarrierFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Negative {
    words: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CarrierFile {
    check: String,
    patterns: Vec<String>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    hosts: Vec<String>,
    track_url: Option<String>,
    #[serde(default)]
    track_urls: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Scores {
    pub keyword: u16,
    pub host: u16,
    pub keyword_and_host: u16,
    pub no_check_digit: u16,
    pub likely: u16,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Context {
    pub keyword_window: usize,
    pub negative_window: usize,
    pub max_group_words: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shape {
    Letter,
    Digit,
    Either,
    Exact(u8),
}

impl Shape {
    pub(crate) fn fits(self, b: u8) -> bool {
        match self {
            Shape::Letter => b.is_ascii_uppercase(),
            Shape::Digit => b.is_ascii_digit(),
            Shape::Either => b.is_ascii_uppercase() || b.is_ascii_digit(),
            Shape::Exact(e) => b == e,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Keyword {
    pub written: String,
    pub folded: Vec<char>,
}

impl Keyword {
    fn new(written: &str) -> Option<Self> {
        let folded = fold(written.trim());
        (!folded.is_empty()).then(|| Self {
            written: written.trim().to_owned(),
            folded,
        })
    }
}

#[derive(Debug)]
pub(crate) struct Carrier {
    pub id: CarrierId,
    pub check: String,
    pub patterns: Vec<Vec<Shape>>,
    pub keywords: Vec<Keyword>,
    pub hosts: Vec<String>,
    pub track_url: Option<String>,
    pub track_urls: BTreeMap<String, String>,
}

impl Carrier {
    /// The tracking page for `number`, from the template only.
    pub(crate) fn track_url(&self, number: &str) -> Option<Url> {
        let template = match &self.track_url {
            Some(template) => template,
            None => {
                let country = number.get(number.len().checked_sub(2)?..)?;
                self.track_urls.get(country)?
            }
        };
        let url = Url::parse(&template.replace("{number}", number)).ok()?;
        (url.scheme() == UrlScheme::Https).then_some(url)
    }
}

#[derive(Debug)]
pub(crate) struct Data {
    pub scores: Scores,
    pub context: Context,
    pub keywords: Vec<Keyword>,
    pub negative: Vec<Keyword>,
    pub carriers: Vec<Carrier>,
    pub max_len: usize,
    pub version: String,
}

pub(crate) static DATA: LazyLock<Option<Arc<Data>>> =
    LazyLock::new(|| Data::parse(SOURCE).ok().map(Arc::new));

/// The version of the matching and scoring code; the data hash is appended to it.
pub const ALGORITHM: u32 = 1;

fn template_ok(template: &str) -> bool {
    template.starts_with("https://")
        && template.matches("{number}").count() == 1
        && Url::parse(&template.replace("{number}", "RA123456785RU"))
            .is_ok_and(|url| url.scheme() == UrlScheme::Https)
}

fn pattern(text: &str) -> Result<Vec<Shape>, String> {
    if text.is_empty() || text.len() > MAX_PATTERN_CHARS {
        return Err(format!("pattern of {} chars", text.len()));
    }
    text.bytes()
        .map(|b| match b {
            b'A' => Ok(Shape::Letter),
            b'D' => Ok(Shape::Digit),
            b'X' => Ok(Shape::Either),
            b if b.is_ascii_uppercase() || b.is_ascii_digit() => Ok(Shape::Exact(b)),
            _ => Err("pattern char outside A-Z and 0-9".to_owned()),
        })
        .collect()
}

impl Data {
    pub(crate) fn parse(source: &str) -> Result<Self, String> {
        let file: File = toml::from_str(source).map_err(|e| e.to_string())?;
        let scores = file.scores;
        if [
            scores.keyword,
            scores.host,
            scores.keyword_and_host,
            scores.no_check_digit,
            scores.likely,
        ]
        .iter()
        .any(|s| *s > 999)
        {
            return Err("scores must stay below 1000".into());
        }
        if file.context.max_group_words == 0 || file.context.max_group_words > 16 {
            return Err("max_group_words outside 1..=16".into());
        }
        let keywords: Vec<Keyword> = file
            .keywords
            .values()
            .flatten()
            .filter_map(|w| Keyword::new(w))
            .collect();
        if keywords.is_empty() {
            return Err("no keywords".into());
        }
        let negative = file
            .negative
            .words
            .iter()
            .filter_map(|w| Keyword::new(w))
            .collect();

        let mut carriers = Vec::new();
        let mut max_len = 0;
        for (id, carrier) in file.carriers {
            let carrier_id =
                CarrierId::new(&id).map_err(|_| format!("carrier id {id:?} is not [a-z0-9_]"))?;
            if !SCHEMES.contains(&carrier.check.as_str()) {
                return Err(format!("{id}: unknown check {:?}", carrier.check));
            }
            if carrier.patterns.is_empty() {
                return Err(format!("{id}: no patterns"));
            }
            let patterns = carrier
                .patterns
                .iter()
                .map(|p| pattern(p).map_err(|e| format!("{id}: {e}")))
                .collect::<Result<Vec<_>, _>>()?;
            max_len = patterns.iter().map(Vec::len).fold(max_len, usize::max);
            let templates = carrier.track_url.iter().chain(carrier.track_urls.values());
            if let Some(bad) = templates.into_iter().find(|t| !template_ok(t)) {
                return Err(format!("{id}: bad tracking template {bad:?}"));
            }
            if carrier
                .track_urls
                .keys()
                .any(|k| k.len() != 2 || !k.bytes().all(|b| b.is_ascii_uppercase()))
            {
                return Err(format!("{id}: track_urls keys are two uppercase letters"));
            }
            carriers.push(Carrier {
                id: carrier_id,
                check: carrier.check,
                patterns,
                keywords: carrier
                    .keywords
                    .iter()
                    .filter_map(|w| Keyword::new(w))
                    .collect(),
                hosts: carrier
                    .hosts
                    .iter()
                    .map(|h| h.trim().trim_end_matches('.').to_ascii_lowercase())
                    .filter(|h| !h.is_empty())
                    .collect(),
                track_url: carrier.track_url,
                track_urls: carrier.track_urls,
            });
        }
        if carriers.is_empty() {
            return Err("no carriers".into());
        }

        let hash = blake3::hash(source.as_bytes()).to_hex();
        let short = hash.as_str().get(..12).unwrap_or_default();
        Ok(Self {
            scores,
            context: file.context,
            keywords,
            negative,
            carriers,
            max_len,
            version: format!("{ALGORITHM}+{short}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_data_parses() {
        let data = Data::parse(SOURCE).unwrap();
        assert!(data.version.starts_with("1+"));
        assert_eq!(data.version.len(), 14);
        let ids: Vec<&str> = data.carriers.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            ["dhl_express", "fedex", "russian_post", "s10", "ups", "usps"]
        );
        assert_eq!(data.max_len, 22);
        assert!(data.keywords.iter().any(|k| k.written == "трек-номер"));
    }

    #[test]
    fn templates_build_https_urls_for_known_countries() {
        let data = Data::parse(SOURCE).unwrap();
        let s10 = data
            .carriers
            .iter()
            .find(|c| c.id.as_str() == "s10")
            .unwrap();
        for number in [
            "RA123456785RU",
            "LX908172638US",
            "CP473829103DE",
            "EE582039171CN",
        ] {
            assert!(s10.track_url(number).is_some(), "{number}");
        }
        assert!(s10.track_url("RA123456785ZZ").is_none());
        let ups = data
            .carriers
            .iter()
            .find(|c| c.id.as_str() == "ups")
            .unwrap();
        assert_eq!(
            ups.track_url("1Z999AA10123456784").unwrap().as_str(),
            "https://www.ups.com/track?tracknum=1Z999AA10123456784"
        );
    }

    #[test]
    fn broken_tables_are_refused() {
        for (from, to) in [
            ("check = \"ups\"", "check = \"royal\""),
            ("[carriers.ups]", "[carriers.UPS]"),
            (
                "https://www.ups.com/track?tracknum={number}",
                "http://www.ups.com/{number}",
            ),
            (
                "https://www.ups.com/track?tracknum={number}",
                "https://www.ups.com/track",
            ),
            (
                "patterns = [\"1ZXXXXXXXXXXXXXXXX\"]",
                "patterns = [\"1z-x\"]",
            ),
            (
                "RU = \"https://www.pochta.ru",
                "Ru = \"https://www.pochta.ru",
            ),
            ("likely = 700", "likely = 1000"),
            ("max_group_words = 8", "max_group_words = 0"),
        ] {
            assert!(SOURCE.contains(from), "{from}");
            assert!(Data::parse(&SOURCE.replacen(from, to, 1)).is_err(), "{to}");
        }
    }
}
