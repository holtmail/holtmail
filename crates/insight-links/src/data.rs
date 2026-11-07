use std::collections::BTreeMap;
use std::sync::LazyLock;

use holtmail_insight::LinkAction;
use serde::Deserialize;

use crate::matcher::Matcher;

pub(crate) const SOURCE: &str = include_str!("../data/link-actions.toml");

pub(crate) const LANGUAGES: [&str; 12] = [
    "en", "ru", "de", "fr", "es", "pt", "it", "zh", "ja", "ko", "tr", "pl",
];

pub(crate) const ACTIONS: [(&str, LinkAction); 10] = [
    ("verify", LinkAction::Verify),
    ("confirm", LinkAction::Confirm),
    ("activate", LinkAction::Activate),
    ("reset_password", LinkAction::ResetPassword),
    ("sign_in", LinkAction::SignIn),
    ("magic_link", LinkAction::MagicLink),
    ("view_order", LinkAction::ViewOrder),
    ("track", LinkAction::Track),
    ("check_in", LinkAction::CheckIn),
    ("wallet_save", LinkAction::WalletSave),
];

const ANY: &str = "any";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    weights: Weights,
    labels: BTreeMap<String, BTreeMap<String, Vec<String>>>,
    paths: BTreeMap<String, Vec<String>>,
    wallet: WalletFile,
    social: SocialFile,
    skip: SkipFile,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WalletFile {
    prefixes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SocialFile {
    domains: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SkipFile {
    tokens: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Weights {
    pub label: i32,
    pub aria_label: i32,
    pub button_like: i32,
    pub path_token: i32,
    pub first: i32,
    pub in_footer: i32,
    pub social: i32,
    pub wallet: i32,
    pub likely: i32,
}

/// A URL token and the action it names, if any.
pub(crate) struct PathToken {
    pub token: String,
    pub action: Option<LinkAction>,
}

pub(crate) struct Data {
    pub weights: Weights,
    pub labels: Matcher,
    pub paths: Vec<PathToken>,
    pub wallet: Vec<String>,
    pub social: Vec<String>,
    pub skip: Vec<String>,
    pub version: String,
}

pub(crate) static DATA: LazyLock<Option<Data>> = LazyLock::new(|| Data::parse(SOURCE).ok());

pub(crate) fn action(name: &str) -> Option<LinkAction> {
    ACTIONS
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, action)| *action)
}

fn lower_list(list: Vec<String>, what: &str) -> Result<Vec<String>, String> {
    let out: Vec<String> = list
        .into_iter()
        .map(|item| item.trim().to_lowercase())
        .collect();
    if out.is_empty() || out.iter().any(String::is_empty) {
        return Err(format!("empty entry in {what}"));
    }
    Ok(out)
}

impl Data {
    pub(crate) fn parse(source: &str) -> Result<Self, String> {
        let file: File = toml::from_str(source).map_err(|e| e.to_string())?;
        let mut labels = Vec::new();
        for (name, languages) in &file.labels {
            let action = action(name).ok_or_else(|| format!("unknown action {name}"))?;
            if let Some(extra) = languages.keys().find(|l| !LANGUAGES.contains(&l.as_str())) {
                return Err(format!("unknown language {extra} for {name}"));
            }
            for language in LANGUAGES {
                let words = languages
                    .get(language)
                    .filter(|words| !words.is_empty())
                    .ok_or_else(|| format!("no {language} labels for {name}"))?;
                labels.extend(words.iter().map(|word| (action, word.clone())));
            }
        }
        if let Some((name, _)) = ACTIONS.iter().find(|(n, _)| !file.labels.contains_key(*n)) {
            return Err(format!("no labels for {name}"));
        }

        let mut paths = Vec::new();
        for (name, tokens) in file.paths {
            let action = match name.as_str() {
                ANY => None,
                other => Some(action(other).ok_or_else(|| format!("unknown action {other}"))?),
            };
            for token in lower_list(tokens, "paths")? {
                paths.push(PathToken { token, action });
            }
        }

        let hash = blake3::hash(source.as_bytes()).to_hex();
        let short = hash.as_str().get(..12).unwrap_or_default();
        Ok(Self {
            weights: file.weights,
            labels: Matcher::new(&labels)?,
            paths,
            wallet: lower_list(file.wallet.prefixes, "wallet")?,
            social: lower_list(file.social.domains, "social")?,
            skip: lower_list(file.skip.tokens, "skip")?,
            version: format!("{}+{short}", crate::ALGORITHM),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_data_parses() {
        let data = Data::parse(SOURCE).unwrap();
        assert!(data.version.starts_with(&format!("{}+", crate::ALGORITHM)));
        assert_eq!(data.version.len(), 2 + 12);
        assert_eq!(data.weights.label, 400);
        assert_eq!(data.weights.aria_label, 300);
        assert_eq!(data.weights.button_like, 150);
        assert_eq!(data.weights.path_token, 150);
        assert_eq!(data.weights.first, 50);
        assert_eq!(data.weights.in_footer, -400);
        assert_eq!(data.weights.social, -300);
        assert_eq!(data.weights.likely, 600);
        assert!(
            data.paths
                .iter()
                .any(|p| p.token == "token=" && p.action.is_none())
        );
        assert!(data.social.iter().any(|d| d == "t.me"));
    }

    #[test]
    fn every_action_needs_every_language() {
        let broken = SOURCE.replacen("pl = [\"zaloguj\"", "pl_x = [\"zaloguj\"", 1);
        assert_ne!(broken, SOURCE);
        assert!(Data::parse(&broken).is_err());
        let missing = SOURCE.replacen("[labels.track]", "[labels.track_x]", 1);
        assert!(Data::parse(&missing).is_err());
    }

    #[test]
    fn unknown_path_actions_are_refused() {
        let broken = SOURCE.replacen("any = [", "anything = [", 1);
        assert!(Data::parse(&broken).is_err());
    }

    #[test]
    fn the_version_follows_the_data() {
        let changed = SOURCE.replacen("likely = 600", "likely = 650", 1);
        assert_ne!(
            Data::parse(&changed).unwrap().version,
            Data::parse(SOURCE).unwrap().version
        );
    }
}
