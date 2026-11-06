use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use serde::Deserialize;

use crate::doc::fold_str;
use crate::matcher::{List, Matcher};

pub(crate) const SOURCE: &str = include_str!("../data/code-keywords.toml");

pub(crate) const LANGUAGES: [&str; 12] = [
    "en", "ru", "de", "fr", "es", "pt", "it", "zh", "ja", "ko", "tr", "pl",
];

type PerLanguage = BTreeMap<String, Vec<String>>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    weights: Weights,
    keywords: PerLanguage,
    promo: PerLanguage,
    reference: PerLanguage,
    months: PerLanguage,
    money: MoneyFile,
    address: PerLanguage,
    footer: WordsFile,
    backup: WordsFile,
    words: NeverFile,
    bulk: BulkFile,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MoneyFile {
    symbols: Vec<String>,
    codes: Vec<String>,
    words: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WordsFile {
    words: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NeverFile {
    never: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BulkFile {
    precedence: Vec<String>,
    esp_header_prefixes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Weights {
    pub base: i32,
    pub keyword: i32,
    pub keyword_window: usize,
    pub subject: i32,
    pub emphasis: i32,
    pub alone_on_line: i32,
    pub early: i32,
    pub early_chars: usize,
    pub both_parts: i32,
    pub alphanumeric: i32,
    pub extra_occurrence: i32,
    pub extra_cap: i32,
    pub margin: i32,
    pub likely: i32,
    pub year_like: i32,
    pub gate_chars: usize,
    pub promo_window: usize,
    pub context_tokens: usize,
    pub list_values: usize,
    pub max_occurrences: usize,
}

pub(crate) struct Data {
    pub weights: Weights,
    pub matcher: Matcher,
    pub never: BTreeSet<String>,
    pub keywords: BTreeSet<String>,
    pub precedence: Vec<String>,
    pub esp_prefixes: Vec<String>,
    pub version: String,
}

pub(crate) static DATA: LazyLock<Option<Data>> = LazyLock::new(|| Data::parse(SOURCE).ok());

pub(crate) const ALGORITHM: &str = "2";

impl Data {
    pub(crate) fn parse(source: &str) -> Result<Self, String> {
        let file: File = toml::from_str(source).map_err(|e| e.to_string())?;
        for language in LANGUAGES {
            if file.keywords.get(language).is_none_or(Vec::is_empty) {
                return Err(format!("no keywords for {language}"));
            }
        }

        let mut lists: Vec<(List, Vec<String>)> = Vec::new();
        let flat = |map: &PerLanguage| map.values().flatten().cloned().collect::<Vec<_>>();
        lists.push((List::Code, flat(&file.keywords)));
        lists.push((List::Promo, flat(&file.promo)));
        lists.push((List::Reference, flat(&file.reference)));
        lists.push((List::Month, flat(&file.months)));
        let money = file
            .money
            .symbols
            .iter()
            .chain(&file.money.codes)
            .chain(&file.money.words)
            .cloned()
            .collect();
        lists.push((List::Money, money));
        lists.push((List::Address, flat(&file.address)));
        lists.push((List::Footer, file.footer.words.clone()));
        lists.push((List::Backup, file.backup.words.clone()));

        let keywords = file
            .keywords
            .values()
            .flatten()
            .chain(file.backup.words.iter())
            .map(|w| fold_str(w.trim()))
            .collect();
        let mut never = BTreeSet::new();
        for (_, phrases) in &lists {
            for phrase in phrases {
                for word in fold_str(phrase).split(|c: char| !c.is_alphanumeric()) {
                    if !word.is_empty() {
                        never.insert(word.to_owned());
                    }
                }
            }
        }
        never.extend(file.words.never.iter().map(|w| fold_str(w)));

        let hash = blake3::hash(source.as_bytes()).to_hex();
        let short = hash.as_str().get(..12).unwrap_or_default();
        Ok(Self {
            weights: file.weights,
            matcher: Matcher::new(&lists),
            never,
            keywords,
            precedence: file.bulk.precedence.iter().map(|p| fold_str(p)).collect(),
            esp_prefixes: file
                .bulk
                .esp_header_prefixes
                .iter()
                .map(|p| p.to_ascii_lowercase())
                .collect(),
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
        assert!(data.version.starts_with("2+"));
        assert_eq!(data.version.len(), 14);
        assert!(data.never.contains("code128"));
        assert!(data.never.contains("verification"));
    }

    #[test]
    fn every_language_needs_keywords() {
        let broken = SOURCE.replace("pl = [\n  \"kod\"", "pl_x = [\n  \"kod\"");
        assert!(Data::parse(&broken).is_err());
    }
}
