#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use holtmail_insight::InsightExtractor;
use holtmail_insight_code::CodeExtractor;
use holtmail_insight_links::LinksExtractor;
use holtmail_insight_parcel::ParcelExtractor;
use holtmail_insight_pass::PassExtractor;
use holtmail_insight_schemaorg::SchemaOrgExtractor;
use holtmail_insight_unsubscribe::UnsubscribeExtractor;
use holtmail_insights::Runner;
use holtmail_mime::ParsedMessage;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Labels {
    pub case: Vec<Label>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Label {
    pub file: String,
    pub code: Option<String>,
    pub origin: Option<String>,
    pub negative: Option<String>,
    pub unsubscribe: Option<UnsubscribeLabel>,
    pub links: Option<Vec<LinkLabel>>,
    pub parcel: Option<Vec<ParcelLabel>>,
    pub structured: Option<Vec<StructuredLabel>>,
    pub kind: Option<String>,
    pub topics: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredLabel {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "ref")]
    pub reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkLabel {
    pub action: String,
    pub host: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParcelLabel {
    pub carrier: String,
    pub number: String,
}

#[derive(Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnsubscribeLabel {
    pub https: Option<String>,
    pub http: Option<String>,
    pub mailto: Option<String>,
    #[serde(default)]
    pub one_click: bool,
    pub list_id: Option<String>,
}

pub fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

pub fn labels() -> Vec<Label> {
    let text = std::fs::read_to_string(corpus_dir().join("labels.toml")).unwrap();
    let labels: Labels = toml::from_str(&text).unwrap();
    labels.case
}

/// Every `.eml` file of the corpus, sorted by name.
pub fn files() -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(corpus_dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".eml"))
        .collect();
    out.sort();
    out
}

pub fn parse(file: &str) -> ParsedMessage {
    let raw = std::fs::read(corpus_dir().join(file)).unwrap();
    holtmail_mime::parse(&raw).unwrap_or_else(|_| panic!("{file} does not parse"))
}

/// The runner with every built extractor, in the order the server runs them.
pub fn runner() -> Runner {
    let extractors: Vec<Arc<dyn InsightExtractor>> = vec![
        Arc::new(UnsubscribeExtractor::new()),
        Arc::new(SchemaOrgExtractor::new()),
        Arc::new(PassExtractor::new()),
        Arc::new(ParcelExtractor::new()),
        Arc::new(LinksExtractor::new()),
        Arc::new(CodeExtractor::new()),
    ];
    Runner::new(extractors)
}
